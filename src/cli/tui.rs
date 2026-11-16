use inquire::Select;

use crate::config::VapourConfig;
use crate::error::{VapourError, VapourResult};
use crate::library::{get_installed_games, SortOrder};
use crate::non_steam::load_non_steam_games;

use super::launch::execute_game_launch;
use super::{sort_playable_items, LaunchExecutionOptions, PlayableItem};

/// Dynamically extracts common acronyms and initialisms from a title.
pub fn extract_acronyms(title: &str) -> Vec<String> {
    let mut results = Vec::new();

    let clean = title.replace([':', '-', '_'], " ");
    let words: Vec<&str> = clean.split_whitespace().collect();

    if words.is_empty() {
        return results;
    }

    // 1. First-letter acronym of all words
    let mut all_acronym = String::new();
    for w in &words {
        let alpha_num: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
        if let Some(first) = alpha_num.chars().next() {
            all_acronym.push(first);
        }
        let trailing_digits: String = alpha_num
            .chars()
            .skip(1)
            .filter(|c| c.is_ascii_digit())
            .collect();
        all_acronym.push_str(&trailing_digits);
    }
    if all_acronym.len() >= 2 {
        results.push(all_acronym.to_lowercase());
    }

    // 2. Acronym without common leading articles (the, a, an)
    let filtered_words: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| !matches!(w.to_lowercase().as_str(), "the" | "a" | "an"))
        .collect();

    if filtered_words.len() != words.len() && !filtered_words.is_empty() {
        let mut filtered_acronym = String::new();
        for w in &filtered_words {
            let alpha_num: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            if let Some(first) = alpha_num.chars().next() {
                filtered_acronym.push(first);
            }
            let trailing_digits: String = alpha_num
                .chars()
                .skip(1)
                .filter(|c| c.is_ascii_digit())
                .collect();
            filtered_acronym.push_str(&trailing_digits);
        }
        if filtered_acronym.len() >= 2 && !results.contains(&filtered_acronym.to_lowercase()) {
            results.push(filtered_acronym.to_lowercase());
        }
    }

    // 3. For multi-part titles separated by colon or dash
    if title.contains(':') || title.contains('-') {
        for part in title.split([':', '-']) {
            let part_words: Vec<&str> = part.split_whitespace().collect();
            if part_words.len() >= 2 {
                let mut part_acr = String::new();
                for w in part_words {
                    if let Some(first) = w.chars().find(|c| c.is_alphanumeric()) {
                        part_acr.push(first);
                    }
                }
                if part_acr.len() >= 2 && !results.contains(&part_acr.to_lowercase()) {
                    results.push(part_acr.to_lowercase());
                }
            }
        }
    }

    results
}

pub fn run_interactive_selector(
    config: &VapourConfig,
    opts: &LaunchExecutionOptions,
    sort_order: SortOrder,
) -> VapourResult<()> {
    let steam_games = get_installed_games(false).unwrap_or_default();
    let non_steam_games = load_non_steam_games(config);

    let mut items: Vec<PlayableItem> = steam_games.into_iter().map(PlayableItem::Steam).collect();
    items.extend(non_steam_games.into_iter().map(PlayableItem::NonSteam));

    if items.is_empty() {
        return Err(VapourError::Runtime(
            "No games found. Mount your libraries or add a game with 'vapour add'.".into(),
        ));
    }

    sort_playable_items(&mut items, sort_order);

    let item_scorer =
        |input: &str, item: &PlayableItem, _str_val: &str, _idx: usize| -> Option<i64> {
            let q = input.trim().to_lowercase();
            if q.is_empty() {
                return Some(100);
            }
            let (name, appid) = match item {
                PlayableItem::Steam(g) => (&g.name, g.appid),
                PlayableItem::NonSteam(ng) => (&ng.name, ng.appid),
            };
            let name_lower = name.to_lowercase();
            let acronyms = extract_acronyms(name);
            if acronyms.iter().any(|ac| ac.to_lowercase() == q) {
                return Some(200);
            }
            if name_lower.contains(&q) {
                return Some(100);
            }
            if appid.to_string().contains(&q) {
                return Some(80);
            }
            None
        };

    let selected = match Select::new("Select game to launch:", items)
        .with_scorer(&item_scorer)
        .prompt()
    {
        Ok(i) => i,
        _ => return Ok(()),
    };

    let query = match selected {
        PlayableItem::Steam(ref g) => g.appid.to_string(),
        PlayableItem::NonSteam(ref ng) => ng.appid.to_string(),
    };

    execute_game_launch(config, &query, opts)
}
