//! Where the words are.
//!
//! Selection needs more than text: it needs each word's rectangle in page
//! space, so a click can be turned into a position in the text and a drag can
//! be turned into a range. A pipeline that can produce this can be selected
//! from; one that cannot simply returns `None` and the viewer stays read-only in
//! the literal sense.
//!
//! Coordinates are points from the top-left of the page, which makes them
//! independent of zoom — a layer is computed once per page, not per rung of the
//! ladder.

/// One word, and where it sits on the page.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Word {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

/// Every word on one page, in reading order.
#[derive(Debug, Clone, Default)]
pub struct TextLayer {
    pub words: Vec<Word>,
}

impl TextLayer {
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// The word under a point in page space, if any.
    pub fn word_at(&self, x: f32, y: f32) -> Option<usize> {
        self.words.iter().position(|w| w.contains(x, y))
    }

    /// The nearest word to a point, preferring the one whose vertical band
    /// contains it — so a drag that wanders between lines still resolves.
    pub fn nearest_word(&self, x: f32, y: f32) -> Option<usize> {
        if let Some(index) = self.word_at(x, y) {
            return Some(index);
        }
        let mut best: Option<(usize, f32)> = None;
        for (index, word) in self.words.iter().enumerate() {
            let cy = word.y + word.height / 2.0;
            let dy = (cy - y).abs();
            if dy > word.height {
                continue;
            }
            let dx = if x < word.x {
                word.x - x
            } else if x > word.x + word.width {
                x - (word.x + word.width)
            } else {
                0.0
            };
            let distance = dx + dy * 4.0;
            if best.map(|(_, b)| distance < b).unwrap_or(true) {
                best = Some((index, distance));
            }
        }
        best.map(|(index, _)| index)
    }

    /// The selected text between two word indices, inclusive.
    ///
    /// A new line wherever the vertical position changes, so a copied paragraph
    /// pastes as a paragraph rather than one long line.
    pub fn text_between(&self, a: usize, b: usize) -> String {
        if self.words.is_empty() {
            return String::new();
        }
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let end = end.min(self.words.len() - 1);

        let mut out = String::new();
        let mut previous_y: Option<f32> = None;

        for index in start..=end {
            let Some(word) = self.words.get(index) else {
                continue;
            };
            if let Some(py) = previous_y {
                let same_line = (py - word.y).abs() < word.height * 0.5;
                if !same_line {
                    out.push('\n');
                } else if !out.is_empty() {
                    out.push(' ');
                }
            }
            out.push_str(&word.text);
            previous_y = Some(word.y);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer() -> TextLayer {
        TextLayer {
            words: vec![
                Word { text: "The".into(), x: 10.0, y: 10.0, width: 20.0, height: 12.0 },
                Word { text: "quick".into(), x: 34.0, y: 10.0, width: 30.0, height: 12.0 },
                Word { text: "brown".into(), x: 68.0, y: 10.0, width: 32.0, height: 12.0 },
                Word { text: "fox".into(), x: 10.0, y: 26.0, width: 18.0, height: 12.0 },
            ],
        }
    }

    #[test]
    fn a_point_inside_a_word_finds_it() {
        let l = layer();
        assert_eq!(l.word_at(15.0, 15.0), Some(0));
        assert_eq!(l.word_at(40.0, 15.0), Some(1));
        assert_eq!(l.word_at(500.0, 500.0), None);
    }

    #[test]
    fn selection_joins_with_spaces_within_a_line_and_newlines_between() {
        let l = layer();
        assert_eq!(l.text_between(0, 2), "The quick brown");
        assert_eq!(l.text_between(0, 3), "The quick brown\nfox");
        assert_eq!(l.text_between(3, 0), "The quick brown\nfox");
    }

    #[test]
    fn a_single_word_selects_just_that_word() {
        assert_eq!(layer().text_between(1, 1), "quick");
    }

    #[test]
    fn indices_past_the_end_are_clamped() {
        assert_eq!(layer().text_between(2, 99), "brown\nfox");
    }

    #[test]
    fn nearest_word_snaps_to_the_same_line() {
        let l = layer();
        // In the gap between "The" and "quick", on the first line.
        assert_eq!(l.nearest_word(32.0, 15.0), Some(0));
        // Far below everything: no answer rather than a wrong one.
        assert_eq!(l.nearest_word(10.0, 400.0), None);
    }
}
