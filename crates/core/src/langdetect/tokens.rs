//! Tokenization: rel_path -> segments -> tokens.
//!
//! Everything is lowercased and byte-offset-tracked within its segment so
//! that recognized language tokens can later be excised to build a "shape"
//! string for the language-family heuristic (`family.rs`).

/// Characters that always split a segment into atomic tokens.
const STRONG_DELIMS: [char; 8] = ['_', '-', '.', '(', ')', '[', ']', ' '];
/// Characters that split a segment into "compound candidates" while
/// preserving `-`/`_` so that locale tags like `es-mx` or `pt_br` survive as
/// a single candidate and can match a Level A literal alias directly.
const WEAK_DELIMS: [char; 6] = ['.', '(', ')', '[', ']', ' '];

/// One atomic or compound token/piece found within a segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct Segment {
    /// Lowercased segment text; all offsets below are byte offsets into this.
    pub lower: String,
    /// Index of this segment within the path (0-based).
    pub index: usize,
    /// True if this is the last segment (the file name + extension).
    pub is_filename: bool,
    /// Fully split tokens: strong-delimiter split + CamelCase boundary split.
    pub atoms: Vec<Piece>,
    /// Weakly split pieces: only `. ( ) [ ]` are delimiters, so `-`/`_`
    /// separated locale tags (`es-mx`, `pt_br`) remain intact.
    pub weak_pieces: Vec<Piece>,
}

/// Splits a `rel_path` (using `\` or `/` separators) into non-empty segments.
pub fn split_path_segments(rel_path: &str) -> Vec<&str> {
    rel_path
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extracts the lowercase file extension (without the dot) of `rel_path`, if
/// any. Only looks at the final path segment.
pub fn file_extension(rel_path: &str) -> Option<String> {
    let segments = split_path_segments(rel_path);
    let filename = segments.last()?;
    let dot = filename.rfind('.')?;
    if dot == 0 || dot == filename.len() - 1 {
        return None;
    }
    Some(filename[dot + 1..].to_lowercase())
}

/// Splits `text` on the given delimiter set, returning non-empty pieces with
/// byte offsets into `text`.
fn split_on(text: &str, delims: &[char]) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in text.char_indices() {
        if delims.contains(&ch) {
            if idx > start {
                pieces.push(Piece {
                    text: text[start..idx].to_string(),
                    start,
                    end: idx,
                });
            }
            start = idx + ch.len_utf8();
        }
    }
    if start < text.len() {
        pieces.push(Piece {
            text: text[start..].to_string(),
            start,
            end: text.len(),
        });
    }
    pieces
}

/// Splits a single strong-delimiter piece further on CamelCase boundaries
/// (lowercase/digit -> uppercase transition), operating on the *original*
/// (not-yet-lowercased) text so the case transitions are visible. Returns
/// pieces with offsets relative to `original`.
fn split_camel(original: &str) -> Vec<(usize, usize)> {
    let chars: Vec<(usize, char)> = original.char_indices().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut bounds = vec![0usize];
    for i in 1..chars.len() {
        let (idx, cur) = chars[i];
        let (_, prev) = chars[i - 1];
        if prev.is_lowercase() && cur.is_uppercase() {
            bounds.push(idx);
        }
    }
    bounds.push(original.len());
    bounds.windows(2).map(|w| (w[0], w[1])).collect()
}

/// Tokenizes one path segment: strong-delimiter split, then CamelCase split
/// within each piece. Also computes the weak-delimiter pieces used for
/// locale-tag / literal compound matching.
fn tokenize_segment(raw: &str) -> (Vec<Piece>, Vec<Piece>) {
    // CamelCase boundaries must be computed on the original-case text before
    // lowercasing (lowercasing erases the case transitions).
    let strong_pieces_raw = split_on(raw, &STRONG_DELIMS);
    let mut atoms = Vec::new();
    for piece in &strong_pieces_raw {
        for (rel_start, rel_end) in split_camel(&piece.text) {
            let sub = &piece.text[rel_start..rel_end];
            atoms.push(Piece {
                text: sub.to_lowercase(),
                start: piece.start + rel_start,
                end: piece.start + rel_end,
            });
        }
    }

    let lower = raw.to_lowercase();
    let weak_pieces = split_on(&lower, &WEAK_DELIMS);

    (atoms, weak_pieces)
}

/// Tokenizes a full `rel_path` into segments with atomic and weak-split
/// pieces, all lowercased with byte offsets relative to each segment's
/// lowercased text.
pub fn tokenize_path(rel_path: &str) -> Vec<Segment> {
    let raw_segments = split_path_segments(rel_path);
    let last_idx = raw_segments.len().saturating_sub(1);
    raw_segments
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let (atoms, weak_pieces) = tokenize_segment(raw);
            Segment {
                lower: raw.to_lowercase(),
                index,
                is_filename: index == last_idx,
                atoms,
                weak_pieces,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_camel_case_boundaries() {
        let (atoms, _) = tokenize_segment("SpanishNation.txt");
        let texts: Vec<&str> = atoms.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["spanish", "nation", "txt"]);
    }

    #[test]
    fn weak_pieces_preserve_hyphen_locale_tags() {
        let (_, weak) = tokenize_segment("Spanish(Spain)_patch_1.snd");
        let texts: Vec<&str> = weak.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["spanish", "spain", "_patch_1", "snd"]);
    }

    #[test]
    fn extension_extraction() {
        assert_eq!(
            file_extension("a\\b\\intro_german.bik"),
            Some("bik".to_string())
        );
        assert_eq!(file_extension("a\\b\\noext"), None);
    }
}
