//! fzf-style fuzzy matching: the query's characters must appear in the text in order (case
//! ignored), and matches score higher when they're consecutive, start a word, start the text,
//! or form an exact substring; gaps cost a little. So `iss` finds `ISS (ZARYA)` first and
//! `sl5859` finds `STARLINK-5859`.

/// How well `query` matches `text` (higher is better), or None when it doesn't. Spaces in the
/// query are ignored; an empty query matches everything with 0.
pub fn score(query: &str, text: &str) -> Option<i32> {
    matches(query, text).map(|(s, _)| s)
}

/// [`score`] and where the query's characters matched: indices into `text`'s characters (for
/// highlighting them, as fzf does). Empty for an empty query.
pub fn matches(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let word_start = |i: usize| i == 0 || !t[i - 1].is_alphanumeric() || (t[i].is_ascii_digit() && !t[i - 1].is_ascii_digit());
    // Each query character at its earliest place after the last, preferring a word start
    // (or straight after the last match) when one comes before the next plain match.
    let mut s = 0i32;
    let mut at = 0usize;
    let mut last: Option<usize> = None;
    let mut hits = Vec::with_capacity(q.len());
    for &c in &q {
        let mut found = None;
        for (i, &tc) in t.iter().enumerate().skip(at) {
            if tc != c {
                continue;
            }
            if last.is_some_and(|l| i == l + 1) || word_start(i) {
                found = Some(i);
                break;
            }
            found.get_or_insert(i);
            // Look a little further for a better place, not to the end of a long name.
            if i > found.unwrap_or(i) + 8 {
                break;
            }
        }
        let i = found?;
        s += 1;
        match last {
            Some(l) if i == l + 1 => s += 6,
            Some(l) => s -= ((i - l - 1) as i32).min(6),
            None => s -= (i as i32).min(4),
        }
        if word_start(i) {
            s += 4;
        }
        last = Some(i);
        hits.push(i);
        at = i + 1;
    }
    // A plain substring (spaces and case aside) is what was meant, likely.
    let qs: String = q.iter().collect();
    let ts: String = t.iter().filter(|c| !c.is_whitespace()).collect();
    if ts.contains(&qs) {
        s += 10;
        if ts.starts_with(&qs) {
            s += 6;
        }
    }
    // Positions in the lowercased text are the text's own unless lowercasing changed its
    // length (a few non-ASCII letters): then none are shown.
    if t.len() != text.chars().count() {
        hits.clear();
    }
    // Among equals, shorter names first.
    Some((s * 4 - (t.len() as i32 / 8), hits))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_for_highlighting() {
        assert_eq!(matches("usg", "usgs").unwrap().1, vec![0, 1, 2]);
        assert_eq!(matches("ei", "esri imagery").unwrap().1, vec![0, 5]);
        assert_eq!(matches("", "esri").unwrap().1, Vec::<usize>::new());
        assert!(matches("zz", "esri").is_none());
    }

    fn best<'a>(q: &str, names: &[&'a str]) -> Vec<&'a str> {
        let mut v: Vec<(i32, &str)> = names.iter().filter_map(|n| score(q, n).map(|s| (s, *n))).collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        v.into_iter().map(|(_, n)| n).collect()
    }

    #[test]
    fn subsequences_in_order() {
        assert!(score("iss", "ISS (ZARYA)").is_some());
        assert!(score("zarya", "ISS (ZARYA)").is_some());
        assert!(score("ayraz", "ISS (ZARYA)").is_none(), "order matters");
        assert!(score("issx", "ISS (ZARYA)").is_none());
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn ranks_like_fzf() {
        let names = ["ISS (ZARYA)", "SWISSCUBE", "CSS (TIANHE)", "STARLINK-5859", "STARLINK-1585", "NOAA 19", "METEOR-M2 3", "TIANZHOU-9"];
        assert_eq!(best("iss", &names)[0], "ISS (ZARYA)");
        assert_eq!(best("sl5859", &names)[0], "STARLINK-5859");
        assert_eq!(best("5859", &names)[0], "STARLINK-5859");
        assert_eq!(best("noaa", &names)[0], "NOAA 19");
        assert_eq!(best("tian", &names)[0], "TIANZHOU-9", "a word start beats a later one");
        assert_eq!(best("meteor m2", &names)[0], "METEOR-M2 3");
    }
}
