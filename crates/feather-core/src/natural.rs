//! Ordering names the way a reader expects.
//!
//! `page10` belongs after `page9`, not after `page1`. Plain lexicographic order
//! gets that wrong, which in a comic is a page out of sequence and in a scanned
//! book is a chapter out of order. Both the comic pipeline and the EPUB pipeline
//! need this, so it lives here rather than twice.

use std::cmp::Ordering;

/// Compare two names, treating runs of digits as numbers.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();

    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ac), Some(bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let an = take_digits(&mut ai);
                    let bn = take_digits(&mut bi);
                    let av: u128 = an.parse().unwrap_or(0);
                    let bv: u128 = bn.parse().unwrap_or(0);
                    match av.cmp(&bv) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                } else if ac == bc {
                    ai.next();
                    bi.next();
                } else {
                    return ac.cmp(&bc);
                }
            }
        }
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut out = String::new();
    while let Some(c) = it.peek().copied() {
        if c.is_ascii_digit() {
            out.push(c);
            it.next();
        } else {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_compare_as_numbers() {
        let mut names = vec!["p10.png", "p2.png", "p1.png", "p20.png", "p9.png"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["p1.png", "p2.png", "p9.png", "p10.png", "p20.png"]);
    }

    #[test]
    fn text_still_orders_alphabetically() {
        assert_eq!(natural_cmp("cover.xhtml", "page1.xhtml"), Ordering::Less);
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);
        assert_eq!(natural_cmp("b", "a"), Ordering::Greater);
    }

    #[test]
    fn a_prefix_sorts_before_its_extension() {
        assert_eq!(natural_cmp("ch1", "ch1.xhtml"), Ordering::Less);
    }
}
