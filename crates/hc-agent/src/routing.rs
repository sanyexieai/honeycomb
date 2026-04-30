use std::collections::BTreeSet;

pub fn phrase_match_score(input: &str, phrase: &str) -> i32 {
    let input = input.trim();
    let phrase = phrase.trim();
    if input.is_empty() || phrase.is_empty() {
        return 0;
    }

    let input_lower = input.to_lowercase();
    let phrase_lower = phrase.to_lowercase();
    if input_lower.contains(&phrase_lower) || phrase_lower.contains(&input_lower) {
        return 100;
    }

    let input_terms = route_match_terms(input);
    let phrase_terms = route_match_terms(phrase);
    if input_terms.is_empty() || phrase_terms.is_empty() {
        return 0;
    }

    let overlap = phrase_terms
        .iter()
        .filter(|term| input_terms.contains(*term))
        .count();
    if overlap == 0 {
        return 0;
    }

    let coverage = overlap as f32 / phrase_terms.len() as f32;
    if coverage < 0.15 {
        return 0;
    }
    (coverage * 90.0).round() as i32
}

pub fn best_phrase_match_score<'a>(
    input: &str,
    phrases: impl IntoIterator<Item = &'a String>,
) -> i32 {
    phrases
        .into_iter()
        .map(|phrase| phrase_match_score(input, phrase))
        .max()
        .unwrap_or(0)
}

pub fn route_match_terms(text: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    let lowered = text.to_lowercase();
    for token in lowered.split(|ch: char| !ch.is_alphanumeric()) {
        if token.chars().count() > 1 {
            terms.insert(token.to_owned());
        }
    }

    for run in text
        .split(|ch: char| ch.is_ascii() || ch.is_whitespace() || ch.is_ascii_punctuation())
        .filter(|part| !part.is_empty())
    {
        let chars: Vec<char> = run.chars().collect();
        if chars.len() > 1 {
            terms.insert(chars.iter().collect::<String>().to_lowercase());
        }
        for size in [2usize, 3usize] {
            for window in chars.windows(size) {
                terms.insert(window.iter().collect::<String>().to_lowercase());
            }
        }
    }
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_score_handles_cjk_examples_without_exact_substring() {
        assert!(phrase_match_score("中午推荐我吃什么", "推荐午餐") > 0);
        assert_eq!(phrase_match_score("中午推荐我吃什么", "播放有声书"), 0);
    }

    #[test]
    fn phrase_score_handles_ascii_case_insensitive_match() {
        assert_eq!(
            phrase_match_score("Please recommend lunch", "recommend"),
            100
        );
    }
}
