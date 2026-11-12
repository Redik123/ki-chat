//! Les petites mises en forme : heures, jours, durées, tailles, textes coupés.
//!
//! Des fonctions pures, sans état ni interface, que tout le client appelle.

use chrono::{Datelike, TimeZone};

/// « 3:56 » — une durée en minutes et secondes.
pub(crate) fn mmss(secondes: u64) -> String {
    format!("{}:{:02}", secondes / 60, secondes % 60)
}

/// « il y a 3 min », « il y a 2 h », « il y a 5 j » — pour dater une
/// fiche ou un match sans afficher d'horodatage.
pub(crate) fn il_y_a(ms: u64) -> String {
    let maintenant = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let s = maintenant.saturating_sub(ms) / 1000;
    match s {
        0..=59 => "à l'instant".to_string(),
        60..=3599 => format!("il y a {} min", s / 60),
        3600..=86_399 => format!("il y a {} h", s / 3600),
        _ => format!("il y a {} j", s / 86_400),
    }
}

pub(crate) fn format_time(ts_millis: u64) -> String {
    chrono::Local
        .timestamp_millis_opt(ts_millis as i64)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// Heure avec les secondes — le journal audio se joue à la seconde près.
pub(crate) fn format_time_secs(ts_millis: u64) -> String {
    chrono::Local
        .timestamp_millis_opt(ts_millis as i64)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Numéro de jour absolu — sert à détecter le changement de date.
pub(crate) fn day_key(ts_millis: u64) -> i32 {
    chrono::Local
        .timestamp_millis_opt(ts_millis as i64)
        .single()
        .map(|t| t.date_naive().num_days_from_ce())
        .unwrap_or(i32::MIN)
}

/// « Aujourd'hui », « Hier », ou la date — sans dépendre de la locale.
pub(crate) fn day_label(ts_millis: u64) -> String {
    let stamp = chrono::Local.timestamp_millis_opt(ts_millis as i64).single();
    let Some(dt) = stamp else { return String::new() };
    let today = chrono::Local::now().date_naive();
    let date = dt.date_naive();
    if date == today {
        "Aujourd'hui".into()
    } else if Some(date) == today.pred_opt() {
        "Hier".into()
    } else {
        date.format("%d/%m/%Y").to_string()
    }
}

/// Tronque un libellé trop long, en comptant en caractères et non en octets.
pub(crate) fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// Raccourcit une URL trop longue pour ne pas noyer le message.
pub(crate) fn shorten(url: &str) -> String {
    let len = url.chars().count();
    if len <= 70 {
        return url.to_string();
    }
    let head: String = url.chars().take(46).collect();
    let tail: String = url.chars().skip(len - 16).collect();
    format!("{head}…{tail}")
}

/// Compte abrégé : 1234 → « 1.2k ».
pub(crate) fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// Un débit lisible, virgule à la française : « 850 kbit/s », « 38,4 Mbit/s ».
pub(crate) fn debit_lisible(kbps: u32) -> String {
    if kbps < 1000 {
        format!("{kbps} kbit/s")
    } else {
        format!("{:.1} Mbit/s", f64::from(kbps) / 1000.0).replace('.', ",")
    }
}

/// Une durée en secondes, dite en jours, heures ou minutes.
pub(crate) fn duree_lisible(secondes: u64) -> String {
    let (j, h, m) = (secondes / 86_400, (secondes / 3600) % 24, (secondes / 60) % 60);
    if j > 0 {
        format!("{j} j {h} h")
    } else if h > 0 {
        format!("{h} h {m:02}")
    } else {
        format!("{m} min")
    }
}

pub(crate) fn megabytes(bytes: u64) -> f32 {
    bytes as f32 / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_duree_se_dit_en_jours_heures_ou_minutes() {
        assert_eq!(duree_lisible(0), "0 min");
        assert_eq!(duree_lisible(59), "0 min");
        assert_eq!(duree_lisible(25 * 60), "25 min");
        assert_eq!(duree_lisible(3600 + 5 * 60), "1 h 05");
        assert_eq!(duree_lisible(3 * 86_400 + 4 * 3600), "3 j 4 h");
    }

    #[test]
    fn long_labels_are_cut_on_character_boundaries() {
        assert_eq!(ellipsize("BAWS", 26), "BAWS");
        // Accents compris : on compte des caractères, pas des octets.
        let long = "Sérveur très très long de Kévin";
        let cut = ellipsize(long, 12);
        assert_eq!(cut.chars().count(), 12);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn urls_are_shortened_without_breaking_utf8() {
        let url = format!("http://exemple.fr/{}", "é".repeat(80));
        let short = shorten(&url);
        assert!(short.chars().count() < url.chars().count());
        assert!(short.contains('…'));
    }
}
