//! Sonde et atelier : rendre le modèle Silero VAD lisible par tract.
//!
//! L'export TorchScript de Silero est truffé de « If » qui ne dépendent que
//! des formes (« l'état est-il vide ? », « le tenseur a-t-il deux ou trois
//! dimensions ? »). tract ne traduit un « If » que si sa condition est
//! constante, et n'en admet pas à deux sorties. Avec nos formes fixes — un
//! bloc de 512 échantillons, un état [2, 1, 128] — chaque condition a une
//! réponse connue : on la calcule (par tract lui-même, sur le graphe
//! tronqué à la condition) et l'on remplace le « If » par sa branche. Le
//! graphe simplifié est écrit à côté du modèle d'origine, et c'est lui que
//! le moteur embarque.
//!
//! `cargo run -p ki-voice --example silero_sonde --release`

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use prost::Message;
use tract_onnx::pb;
use tract_onnx::prelude::*;

const MODELE: &[u8] = include_bytes!("../models/silero/silero_vad_16k_op15.onnx");
const SORTIE: &str = "crates/voice/models/silero/silero_vad_16k_tract.onnx";

/// ONNX : `AttributeProto.type = TENSOR`, `TensorProto.data_type = BOOL`.
const ATTR_TENSOR: i32 = 4;
const TYPE_BOOL: i32 = 9;

/// Remplace, dans un graphe et ses branches, chaque `Cast` vers booléen
/// nommé dans `noms` par une constante `true`.
///
/// Ce sont les conditions « l'état a-t-il une forme non vide » : un `Cast`
/// d'une dimension vers un booléen, que tract ne sait pas évaluer — alors
/// qu'avec nos formes fixes la réponse est toujours oui.
fn forcer_vrai(graphe: &mut pb::GraphProto, noms: &[&str]) -> usize {
    let mut n = 0;
    for node in graphe.node.iter_mut() {
        if node.op_type == "Cast" && noms.contains(&node.name.as_str()) {
            node.op_type = "Constant".into();
            node.input.clear();
            node.attribute = vec![pb::AttributeProto {
                name: "value".into(),
                r#type: ATTR_TENSOR,
                t: Some(pb::TensorProto {
                    data_type: TYPE_BOOL,
                    int32_data: vec![1],
                    ..Default::default()
                }),
                ..Default::default()
            }];
            n += 1;
        }
        for a in node.attribute.iter_mut() {
            if let Some(g) = a.g.as_mut() {
                n += forcer_vrai(g, noms);
            }
        }
    }
    n
}

/// Le graphe réduit à ce qu'il faut pour calculer `tenseur`, qui en devient
/// l'unique sortie.
fn tronquer(graphe: &pb::GraphProto, tenseur: &str) -> pb::GraphProto {
    let producteur: HashMap<&str, usize> = graphe
        .node
        .iter()
        .enumerate()
        .flat_map(|(i, n)| n.output.iter().map(move |o| (o.as_str(), i)))
        .collect();
    let mut garder: HashSet<usize> = HashSet::new();
    let mut pile = vec![tenseur.to_string()];
    while let Some(nom) = pile.pop() {
        if let Some(&i) = producteur.get(nom.as_str()) {
            if garder.insert(i) {
                pile.extend(graphe.node[i].input.iter().cloned());
            }
        }
    }
    let mut copie = graphe.clone();
    copie.node = graphe
        .node
        .iter()
        .enumerate()
        .filter(|(i, _)| garder.contains(i))
        .map(|(_, n)| n.clone())
        .collect();
    copie.output = vec![pb::ValueInfoProto { name: tenseur.into(), ..Default::default() }];
    copie
}

fn charger(proto: &pb::ModelProto) -> TractResult<InferenceModel> {
    // Les formes de sortie déclarées portent des dimensions symboliques
    // jusque dans les branches : on les ignore, elles se déduisent des
    // entrées. La fréquence est une constante.
    tract_onnx::onnx()
        .with_ignore_output_shapes(true)
        .model_for_proto_model(proto)?
        // 576 = 64 échantillons de contexte (la fin du bloc précédent) + 512
        // nouveaux : c'est ainsi que l'enveloppe Python de Silero v5 nourrit
        // le modèle, sans quoi rien ne dépasse 0,2.
        .with_input_fact(0, f32::fact([1, 576]).into())?
        .with_input_fact(1, f32::fact([2, 1, 128]).into())?
        .with_input_fact(2, InferenceFact::from(tensor0(16_000i64)))
}

/// La valeur d'une condition booléenne, calculée par tract sur le graphe
/// tronqué : avec les entrées fixées, tout ce qui ne dépend que des formes
/// se replie en constante.
fn evaluer(proto: &pb::ModelProto, tenseur: &str) -> TractResult<bool> {
    let mut reduit = proto.clone();
    let graphe = reduit.graph.as_ref().expect("graphe");
    reduit.graph = Some(tronquer(graphe, tenseur));
    let modele = charger(&reduit)?.into_optimized()?;
    let sortie = modele.outputs[0];
    let fact = modele.outlet_fact(sortie)?;
    let konst = fact
        .konst
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("la condition {tenseur} ne se replie pas en constante : {fact:?}"))?;
    Ok(*konst.to_scalar::<bool>()?)
}

/// Remplace chaque « If » du graphe principal par la branche que sa
/// condition désigne, jusqu'à ce qu'il n'en reste plus. Les branches d'une
/// branche remontent au passage et se traitent au tour suivant.
fn aplatir(proto: &mut pb::ModelProto) -> TractResult<usize> {
    let mut n = 0;
    loop {
        let graphe = proto.graph.as_ref().expect("graphe");
        let Some(pos) = graphe.node.iter().position(|n| n.op_type == "If") else { break };
        let node = graphe.node[pos].clone();
        let cond = evaluer(proto, &node.input[0])?;
        let branche = node
            .attribute
            .iter()
            .find(|a| a.name == if cond { "then_branch" } else { "else_branch" })
            .and_then(|a| a.g.clone())
            .ok_or_else(|| anyhow::anyhow!("« If » {} sans branche", node.name))?;
        println!(
            "  {} : condition {cond}, branche de {} nœud(s) mise à plat",
            node.name,
            branche.node.len()
        );
        let mut nouveaux = branche.node.clone();
        for (interne, externe) in branche.output.iter().zip(node.output.iter()) {
            nouveaux.push(pb::NodeProto {
                op_type: "Identity".into(),
                name: format!("{}_sortie_{}", node.name, externe),
                input: vec![interne.name.clone()],
                output: vec![externe.clone()],
                ..Default::default()
            });
        }
        let graphe = proto.graph.as_mut().expect("graphe");
        graphe.initializer.extend(branche.initializer.iter().cloned());
        graphe.node.splice(pos..pos + 1, nouveaux);
        n += 1;
    }
    Ok(n)
}

fn main() -> TractResult<()> {
    let mut proto = pb::ModelProto::decode(MODELE)?;
    let patches = forcer_vrai(proto.graph.as_mut().expect("graphe"), &["/model/decoder/Cast"]);
    println!("conditions forcées à vrai : {patches}");
    let ifs = aplatir(&mut proto)?;
    println!("« If » mis à plat : {ifs}");
    let octets = proto.encode_to_vec();
    std::fs::write(SORTIE, &octets)?;
    println!("modèle simplifié écrit : {SORTIE} ({} octets)", octets.len());

    let modele = charger(&proto)?.into_optimized()?.into_runnable()?;
    let entrees = modele.model().inputs.len();
    println!("entrées après optimisation : {entrees}");
    for (i, o) in modele.model().outputs.iter().enumerate() {
        println!("sortie {i} : {:?}", modele.model().outlet_fact(*o)?);
    }

    let mut etat = Tensor::zero::<f32>(&[2, 1, 128])?;
    let silence = Tensor::zero::<f32>(&[1, 576])?;
    let mut bruit = vec![0f32; 576];
    let mut graine: u32 = 12345;
    for x in bruit.iter_mut() {
        graine = graine.wrapping_mul(1664525).wrapping_add(1013904223);
        *x = ((graine >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.2;
    }
    let bruit = Tensor::from_shape(&[1, 576], &bruit)?;
    // Une « voix » de synthèse : une fondamentale à 140 Hz et ses
    // harmoniques, modulée — ce n'est pas de la parole, mais c'est ce qui
    // s'en approche le plus sans fichier audio.
    let mut voix = vec![0f32; 576];
    let mut phase = 0f32;
    for (i, x) in voix.iter_mut().enumerate() {
        phase += 140.0 / 16_000.0;
        let env = 0.5 + 0.5 * ((i as f32 / 576.0) * std::f32::consts::PI).sin();
        *x = env
            * (1..=8)
                .map(|h| (phase * h as f32 * std::f32::consts::TAU).sin() / h as f32)
                .sum::<f32>()
            * 0.3;
    }
    let voix = Tensor::from_shape(&[1, 576], &voix)?;

    for (nom, bloc) in [("silence", &silence), ("bruit blanc", &bruit), ("voix de synthèse", &voix)] {
        let debut = Instant::now();
        let mut derniere = 0f32;
        for _ in 0..30 {
            let mut entrees_run: TVec<TValue> = tvec!(bloc.clone().into(), etat.clone().into());
            if entrees == 3 {
                entrees_run.push(tensor0(16_000i64).into());
            }
            let sorties = modele.run(entrees_run)?;
            derniere = sorties[0].to_array_view::<f32>()?[[0, 0]];
            etat = sorties[1].clone().into_tensor();
        }
        println!(
            "{nom} : probabilité {derniere:.3}, {:.2} ms par bloc de 32 ms",
            debut.elapsed().as_secs_f64() * 1000.0 / 30.0
        );
    }

    // De la vraie parole, si un fichier WAV est donné en argument : le
    // chargeur des effets sonores le ramène en 48 kHz mono, on décime par
    // trois (moyenne de trois échantillons : un passe-bas grossier, assez
    // pour une détection de parole), et l'on suit la probabilité bloc par
    // bloc.
    if let Some(chemin) = std::env::args().nth(1) {
        let pcm48 = ki_voice::effects::load_wav_file(&chemin)
            .map_err(|e| anyhow::anyhow!("{chemin} : {e}"))?;
        let pcm16: Vec<f32> = pcm48.as_chunks::<3>().0.iter().map(|c| (c[0] + c[1] + c[2]) / 3.0).collect();
        let mut etat = Tensor::zero::<f32>(&[2, 1, 128])?;
        let mut frise = String::new();
        let mut hauts = 0usize;
        let mut blocs = 0usize;
        let mut max = 0f32;
        let debut = Instant::now();
        let mut contexte = [0f32; 64];
        let mut entree = vec![0f32; 576];
        for bloc in pcm16.as_chunks::<512>().0 {
            entree[..64].copy_from_slice(&contexte);
            entree[64..].copy_from_slice(bloc);
            contexte.copy_from_slice(&bloc[512 - 64..]);
            let t = Tensor::from_shape(&[1, 576], &entree)?;
            let mut entrees_run: TVec<TValue> = tvec!(t.into(), etat.clone().into());
            if entrees == 3 {
                entrees_run.push(tensor0(16_000i64).into());
            }
            let sorties = modele.run(entrees_run)?;
            let p = sorties[0].to_array_view::<f32>()?[[0, 0]];
            etat = sorties[1].clone().into_tensor();
            frise.push(match p {
                p if p >= 0.8 => '█',
                p if p >= 0.5 => '▓',
                p if p >= 0.2 => '░',
                _ => '·',
            });
            hauts += (p >= 0.5) as usize;
            blocs += 1;
            max = max.max(p);
        }
        let ms = debut.elapsed().as_secs_f64() * 1000.0 / blocs.max(1) as f64;
        println!("parole ({chemin}) : {blocs} blocs, {hauts} au-dessus de 0,5, max {max:.3}, {ms:.2} ms par bloc");
        for ligne in frise.as_bytes().chunks(100) {
            println!("  {}", String::from_utf8_lossy(ligne));
        }
    }
    Ok(())
}
