//! Les jeux que l'on reconnaît à leur exécutable — pour filmer la bonne
//! fenêtre et nommer un clip, et pour dire aux membres « joue à Rocket
//! League » sous le pseudo, comme la partie VALORANT se lit déjà.
//!
//! Rien n'est lu dans le jeu : seule sa fenêtre est regardée, par le même
//! inventaire que le sélecteur de source du partage d'écran. Un jeu absent
//! de la table n'est pas reconnu et ne se dit pas — c'est le seul coût, et
//! la table s'allonge d'une ligne.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

/// (exécutable, en minuscules ; nom affiché). Le premier trouvé gagne.
pub const JEUX: &[(&str, &str)] = &[
    // Les tireurs et les arènes du soir.
    ("valorant-win64-shipping.exe", "VALORANT"),
    ("valorant.exe", "VALORANT"),
    ("cs2.exe", "CS2"),
    ("fortniteclient-win64-shipping.exe", "Fortnite"),
    ("rocketleague.exe", "Rocket League"),
    ("r5apex.exe", "Apex"),
    ("league of legends.exe", "League of Legends"),
    ("overwatch.exe", "Overwatch"),
    ("rainbowsix.exe", "Rainbow Six"),
    ("rainbowsix_vulkan.exe", "Rainbow Six"),
    ("gta5.exe", "GTA V"),
    ("gta5_enhanced.exe", "GTA V"),
    ("marvel-win64-shipping.exe", "Marvel Rivals"),
    ("dota2.exe", "Dota 2"),
    ("project8.exe", "Deadlock"),
    ("tslgame.exe", "PUBG"),
    ("discovery.exe", "The Finals"),
    ("supervive.exe", "Supervive"),
    ("cod.exe", "Call of Duty"),
    ("modernwarfare.exe", "Call of Duty"),
    ("blackopscoldwar.exe", "Call of Duty"),
    ("bf2042.exe", "Battlefield 2042"),
    ("bf6.exe", "Battlefield 6"),
    ("titanfall2.exe", "Titanfall 2"),
    ("deltaforceclient-win64-shipping.exe", "Delta Force"),
    ("escapefromtarkov.exe", "Escape from Tarkov"),
    ("huntgame.exe", "Hunt: Showdown"),
    ("haloinfinite.exe", "Halo Infinite"),
    ("mcc-win64-shipping.exe", "Halo MCC"),
    ("destiny2.exe", "Destiny 2"),
    ("warframe.x64.exe", "Warframe"),
    ("m1-win64-shipping.exe", "The First Descendant"),
    ("thedivision2.exe", "The Division 2"),
    ("grb.exe", "Ghost Recon Breakpoint"),
    ("forhonor.exe", "For Honor"),
    ("squadgame.exe", "Squad"),
    ("hll-win64-shipping.exe", "Hell Let Loose"),
    ("insurgencyclient-win64-shipping.exe", "Insurgency: Sandstorm"),
    ("readyornot-win64-shipping.exe", "Ready or Not"),
    ("payday3client-win64-shipping.exe", "Payday 3"),
    ("payday2_win32_release.exe", "Payday 2"),
    ("arma3_x64.exe", "Arma 3"),
    ("armareforgersteam.exe", "Arma Reforger"),
    ("dayz_x64.exe", "DayZ"),
    ("tf_win64.exe", "Team Fortress 2"),
    ("left4dead2.exe", "Left 4 Dead 2"),
    ("back4blood.exe", "Back 4 Blood"),
    ("hl2.exe", "Half-Life 2"),
    ("portal2.exe", "Portal 2"),
    ("gmod.exe", "Garry's Mod"),
    ("doometernalx64vk.exe", "DOOM Eternal"),
    ("doomthedarkages.exe", "DOOM: The Dark Ages"),
    ("ultrakill.exe", "ULTRAKILL"),
    ("naraka.exe", "Naraka: Bladepoint"),
    ("narakabladepoint.exe", "Naraka: Bladepoint"),
    ("enlisted.exe", "Enlisted"),
    ("aces.exe", "War Thunder"),
    ("worldoftanks.exe", "World of Tanks"),
    ("worldofwarships.exe", "World of Warships"),
    // Les soirées entre copains.
    ("among us.exe", "Among Us"),
    ("phasmophobia.exe", "Phasmophobia"),
    ("lethal company.exe", "Lethal Company"),
    ("content warning.exe", "Content Warning"),
    ("repo.exe", "R.E.P.O."),
    ("peak.exe", "PEAK"),
    ("schedule i.exe", "Schedule I"),
    ("devour.exe", "Devour"),
    ("totclient-win64-shipping.exe", "The Outlast Trials"),
    ("deadbydaylight-win64-shipping.exe", "Dead by Daylight"),
    ("fallguys_client_game.exe", "Fall Guys"),
    ("gang beasts.exe", "Gang Beasts"),
    ("pummelparty.exe", "Pummel Party"),
    ("golf it!.exe", "Golf It!"),
    ("tabletop simulator.exe", "Tabletop Simulator"),
    ("ittakestwo.exe", "It Takes Two"),
    ("splitfiction.exe", "Split Fiction"),
    ("fsd-win64-shipping.exe", "Deep Rock Galactic"),
    ("risk of rain 2.exe", "Risk of Rain 2"),
    ("helldivers2.exe", "Helldivers 2"),
    ("warhammer 40000 space marine 2.exe", "Space Marine 2"),
    ("darktide.exe", "Darktide"),
    ("vermintide2.exe", "Vermintide 2"),
    ("borderlands3.exe", "Borderlands 3"),
    ("borderlands4.exe", "Borderlands 4"),
    ("robloxplayerbeta.exe", "Roblox"),
    ("minecraft.windows.exe", "Minecraft"),
    ("terraria.exe", "Terraria"),
    ("stardew valley.exe", "Stardew Valley"),
    ("corekeeper.exe", "Core Keeper"),
    ("brawlhalla.exe", "Brawlhalla"),
    ("multiversus.exe", "MultiVersus"),
    ("osu!.exe", "osu!"),
    ("geometrydash.exe", "Geometry Dash"),
    // Survie et bacs à sable.
    ("rustclient.exe", "Rust"),
    ("valheim.exe", "Valheim"),
    ("enshrouded.exe", "Enshrouded"),
    ("palworld-win64-shipping.exe", "Palworld"),
    ("sonsoftheforest.exe", "Sons of the Forest"),
    ("theforest.exe", "The Forest"),
    ("greenhell.exe", "Green Hell"),
    ("raft.exe", "Raft"),
    ("subnautica.exe", "Subnautica"),
    ("nms.exe", "No Man's Sky"),
    ("7daystodie.exe", "7 Days to Die"),
    ("arkascended.exe", "ARK: Survival Ascended"),
    ("shootergame.exe", "ARK: Survival Evolved"),
    ("conansandbox.exe", "Conan Exiles"),
    ("vrising.exe", "V Rising"),
    ("once_human.exe", "Once Human"),
    ("maine-win64-shipping.exe", "Grounded"),
    ("abioticfactor-win64-shipping.exe", "Abiotic Factor"),
    ("planet crafter.exe", "The Planet Crafter"),
    ("icarus-win64-shipping.exe", "Icarus"),
    ("factorygame-win64-shipping.exe", "Satisfactory"),
    ("factorio.exe", "Factorio"),
    ("dspgame.exe", "Dyson Sphere Program"),
    ("oxygennotincluded.exe", "Oxygen Not Included"),
    ("rimworldwin64.exe", "RimWorld"),
    ("planetzoo.exe", "Planet Zoo"),
    ("planetcoaster2.exe", "Planet Coaster 2"),
    ("cities2.exe", "Cities: Skylines II"),
    ("anno1800.exe", "Anno 1800"),
    ("anno117.exe", "Anno 117"),
    ("ts4_x64.exe", "Les Sims 4"),
    ("inzoi.exe", "inZOI"),
    ("farmingsimulator2025game.exe", "Farming Simulator 25"),
    ("flightsimulator.exe", "Flight Simulator"),
    ("flightsimulator2024.exe", "Flight Simulator 2024"),
    ("dcs.exe", "DCS World"),
    ("ksp_x64.exe", "Kerbal Space Program"),
    ("beamng.drive.x64.exe", "BeamNG.drive"),
    ("eurotrucks2.exe", "Euro Truck Simulator 2"),
    ("amtrucks.exe", "American Truck Simulator"),
    ("snowrunner.exe", "SnowRunner"),
    ("starcitizen.exe", "Star Citizen"),
    ("elitedangerous64.exe", "Elite Dangerous"),
    // Stratégie.
    ("civilizationvi.exe", "Civilization VI"),
    ("civ7_win64_dx12.exe", "Civilization VII"),
    ("humankind.exe", "Humankind"),
    ("reliccardinal.exe", "Age of Empires IV"),
    ("aoe2de_s.exe", "Age of Empires II"),
    ("warhammer3.exe", "Total War: Warhammer III"),
    ("ck3.exe", "Crusader Kings III"),
    ("eu4.exe", "Europa Universalis IV"),
    ("hoi4.exe", "Hearts of Iron IV"),
    ("stellaris.exe", "Stellaris"),
    ("victoria3.exe", "Victoria 3"),
    ("sc2_x64.exe", "StarCraft II"),
    ("warcraft iii.exe", "Warcraft III"),
    ("reliccoh3.exe", "Company of Heroes 3"),
    ("stormgate.exe", "Stormgate"),
    ("heroesofthestorm_x64.exe", "Heroes of the Storm"),
    ("smite.exe", "Smite"),
    ("2xko.exe", "2XKO"),
    ("hearthstone.exe", "Hearthstone"),
    ("mtga.exe", "Magic: The Gathering Arena"),
    ("balatro.exe", "Balatro"),
    ("slaythespire.exe", "Slay the Spire"),
    ("vampiresurvivors.exe", "Vampire Survivors"),
    // Les mondes persistants.
    ("wow.exe", "World of Warcraft"),
    ("ffxiv_dx11.exe", "Final Fantasy XIV"),
    ("eso64.exe", "The Elder Scrolls Online"),
    ("gw2-64.exe", "Guild Wars 2"),
    ("newworld.exe", "New World"),
    ("lostark.exe", "Lost Ark"),
    ("tl.exe", "Throne and Liberty"),
    ("albion-online.exe", "Albion Online"),
    ("blackdesert64.exe", "Black Desert"),
    ("runescape.exe", "RuneScape"),
    ("dofus.exe", "Dofus"),
    ("wakfu.exe", "Wakfu"),
    ("waven.exe", "Waven"),
    ("pathofexile.exe", "Path of Exile"),
    ("pathofexilesteam.exe", "Path of Exile"),
    ("diablo iv.exe", "Diablo IV"),
    ("last epoch.exe", "Last Epoch"),
    ("grim dawn.exe", "Grim Dawn"),
    ("genshinimpact.exe", "Genshin Impact"),
    ("starrail.exe", "Honkai: Star Rail"),
    ("zenlesszonezero.exe", "Zenless Zone Zero"),
    // Les grandes aventures.
    ("eldenring.exe", "Elden Ring"),
    ("nightreign.exe", "Elden Ring Nightreign"),
    ("darksoulsiii.exe", "Dark Souls III"),
    ("sekiro.exe", "Sekiro"),
    ("armoredcore6.exe", "Armored Core VI"),
    ("lop-win64-shipping.exe", "Lies of P"),
    ("b1-win64-shipping.exe", "Black Myth: Wukong"),
    ("sandfall-win64-shipping.exe", "Clair Obscur: Expedition 33"),
    ("bg3.exe", "Baldur's Gate 3"),
    ("bg3_dx11.exe", "Baldur's Gate 3"),
    ("eocapp.exe", "Divinity: Original Sin 2"),
    ("cyberpunk2077.exe", "Cyberpunk 2077"),
    ("witcher3.exe", "The Witcher 3"),
    ("kingdomcome.exe", "Kingdom Come: Deliverance"),
    ("hogwartslegacy.exe", "Hogwarts Legacy"),
    ("starfield.exe", "Starfield"),
    ("fallout4.exe", "Fallout 4"),
    ("fallout76.exe", "Fallout 76"),
    ("skyrimse.exe", "Skyrim"),
    ("oblivionremastered-win64-shipping.exe", "Oblivion Remastered"),
    ("avowed-win64-shipping.exe", "Avowed"),
    ("jedisurvivor.exe", "Star Wars Jedi: Survivor"),
    ("outlaws.exe", "Star Wars Outlaws"),
    ("starwarsbattlefrontii.exe", "Star Wars Battlefront II"),
    ("thegreatcircle.exe", "Indiana Jones et le Cercle ancien"),
    ("rdr2.exe", "Red Dead Redemption 2"),
    ("hitman3.exe", "Hitman"),
    ("control_dx12.exe", "Control"),
    ("control_dx11.exe", "Control"),
    ("alanwake2.exe", "Alan Wake 2"),
    ("acvalhalla.exe", "Assassin's Creed Valhalla"),
    ("acmirage.exe", "Assassin's Creed Mirage"),
    ("acodyssey.exe", "Assassin's Creed Odyssey"),
    ("acshadows.exe", "Assassin's Creed Shadows"),
    ("farcry6.exe", "Far Cry 6"),
    ("afop.exe", "Avatar: Frontiers of Pandora"),
    ("tlou-i.exe", "The Last of Us Part I"),
    ("tlou-ii.exe", "The Last of Us Part II"),
    ("gow.exe", "God of War"),
    ("gowr.exe", "God of War Ragnarök"),
    ("horizonzerodawn.exe", "Horizon Zero Dawn"),
    ("horizonforbiddenwest.exe", "Horizon Forbidden West"),
    ("spider-man.exe", "Marvel's Spider-Man"),
    ("spider-man2.exe", "Marvel's Spider-Man 2"),
    ("ghostoftsushima.exe", "Ghost of Tsushima"),
    ("riftapart.exe", "Ratchet & Clank: Rift Apart"),
    ("returnal.exe", "Returnal"),
    ("ds.exe", "Death Stranding"),
    ("mgsdelta-win64-shipping.exe", "Metal Gear Solid Δ"),
    ("shproto-win64-shipping.exe", "Silent Hill 2"),
    ("re2.exe", "Resident Evil 2"),
    ("re3.exe", "Resident Evil 3"),
    ("re4.exe", "Resident Evil 4"),
    ("re8.exe", "Resident Evil Village"),
    ("monsterhunterworld.exe", "Monster Hunter: World"),
    ("monsterhunterrise.exe", "Monster Hunter Rise"),
    ("monsterhunterwilds.exe", "Monster Hunter Wilds"),
    ("dd2.exe", "Dragon's Dogma 2"),
    ("devilmaycry5.exe", "Devil May Cry 5"),
    ("streetfighter6.exe", "Street Fighter 6"),
    ("polaris-win64-shipping.exe", "Tekken 8"),
    ("mk12.exe", "Mortal Kombat 1"),
    ("ggst.exe", "Guilty Gear Strive"),
    ("dbfighterz.exe", "Dragon Ball FighterZ"),
    ("sparkingzero-win64-shipping.exe", "Dragon Ball: Sparking! Zero"),
    ("nierautomata.exe", "NieR: Automata"),
    ("p5r.exe", "Persona 5 Royal"),
    ("p3r.exe", "Persona 3 Reload"),
    ("metaphor.exe", "Metaphor: ReFantazio"),
    ("ff7remake_.exe", "Final Fantasy VII Remake"),
    ("ff7rebirth_.exe", "Final Fantasy VII Rebirth"),
    ("ffxvi.exe", "Final Fantasy XVI"),
    ("hades.exe", "Hades"),
    ("hades2.exe", "Hades II"),
    ("deadcells.exe", "Dead Cells"),
    ("hollow_knight.exe", "Hollow Knight"),
    ("hollow knight silksong.exe", "Hollow Knight: Silksong"),
    ("celeste.exe", "Celeste"),
    ("cuphead.exe", "Cuphead"),
    ("oriwotw.exe", "Ori and the Will of the Wisps"),
    ("undertale.exe", "Undertale"),
    ("deltarune.exe", "Deltarune"),
    ("stray.exe", "Stray"),
    ("dead space.exe", "Dead Space"),
    ("davethediver.exe", "Dave the Diver"),
    // Le sport et la route.
    ("fc25.exe", "EA Sports FC 25"),
    ("fc26.exe", "EA Sports FC 26"),
    ("trackmania.exe", "Trackmania"),
    ("forzahorizon5.exe", "Forza Horizon 5"),
    ("needforspeedunbound.exe", "Need for Speed Unbound"),
    ("ac2-win64-shipping.exe", "Assetto Corsa Competizione"),
    ("iracingsim64dx11.exe", "iRacing"),
    ("f1_24.exe", "F1 24"),
    ("f1_25.exe", "F1 25"),
];

/// Le nom du jeu derrière un exécutable, s'il est connu. La casse est
/// indifférente : Windows rend « VALORANT-Win64-Shipping.exe ».
pub fn reconnaitre(exe: &str) -> Option<&'static str> {
    let exe = exe.to_lowercase();
    JEUX.iter().find(|(e, _)| *e == exe).map(|(_, nom)| *nom)
}

/// Le jeu en cours, d'après les fenêtres ouvertes : le premier connu.
pub fn jeu_en_cours() -> Option<&'static str> {
    ki_video::list_windows().iter().find_map(|f| reconnaitre(&f.process))
}

/// La cadence de la ronde : un jeu met plus de cinq secondes à se lancer,
/// et l'inventaire des fenêtres n'est pas gratuit.
const PERIODE: Duration = Duration::from_secs(5);

/// Le fil qui regarde les fenêtres et publie le jeu en cours. Il ne vit que
/// tant que l'option « dire à quoi je joue » est cochée.
pub struct Veilleur {
    jeu: Arc<Mutex<Option<&'static str>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Veilleur {
    pub fn demarrer(ctx: egui::Context) -> Self {
        let jeu = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("ki-jeux".into())
            .spawn({
                let (jeu, stop) = (jeu.clone(), stop.clone());
                move || boucle(ctx, jeu, stop)
            })
            .ok();
        Self { jeu, stop, thread }
    }

    /// Le jeu en cours, tel que la dernière ronde l'a vu.
    pub fn releve(&self) -> Option<&'static str> {
        *self.jeu.lock().unwrap()
    }
}

impl Drop for Veilleur {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn boucle(ctx: egui::Context, jeu: Arc<Mutex<Option<&'static str>>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        let trouve = jeu_en_cours();
        let change = {
            let mut j = jeu.lock().unwrap();
            if *j != trouve {
                *j = trouve;
                true
            } else {
                false
            }
        };
        if change {
            ki_voice::journal(match trouve {
                Some(nom) => format!("jeu reconnu : {nom}"),
                None => "plus de jeu reconnu".to_string(),
            });
            ctx.request_repaint();
        }
        // Dormir par petits pas : décocher l'option ne doit pas attendre la
        // fin de la ronde.
        let depart = Instant::now();
        while depart.elapsed() < PERIODE && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_executable_se_reconnait_quelle_que_soit_sa_casse() {
        assert_eq!(reconnaitre("VALORANT-Win64-Shipping.exe"), Some("VALORANT"));
        assert_eq!(reconnaitre("RocketLeague.exe"), Some("Rocket League"));
        assert_eq!(reconnaitre("explorer.exe"), None);
        assert_eq!(reconnaitre(""), None);
    }

    #[test]
    fn la_table_est_en_minuscules_et_sans_doublon() {
        let mut vus = std::collections::HashSet::new();
        for (exe, nom) in JEUX {
            assert_eq!(*exe, exe.to_lowercase(), "{exe} : à écrire en minuscules");
            assert!(exe.ends_with(".exe"), "{exe} : un exécutable Windows");
            assert!(!nom.is_empty());
            assert!(vus.insert(*exe), "{exe} : en double");
        }
    }
}
