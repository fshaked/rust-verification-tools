#[allow(unused_imports)]
use std;

fn main() {
    for dirs in std::env::args() {
        println!("Santa {} = {}", dirs, santa(&dirs));
        println!(
            "Santa goes underground {} at step {:?}",
            dirs,
            underground_santa(&dirs)
        )
    }
}

fn santa_char(char: char) -> isize {
    match char {
        '(' => 1,
        ')' => -1,
        _ => 0,
    }
}

fn santa(dirs: &str) -> isize {
    dirs.chars().map(santa_char).sum()
    // If we care about overflow in the middle of sum:
    // dirs.chars().map(santa_char).map(BigInt::from).sum::<BigInt>().to_isize().expect("out of range")
}

mod test_part1 {
    #[cfg(not(verify))]
    use proptest::prelude::*;
    #[cfg(verify)]
    use propverify::prelude::*;

    use super::santa;

    #[test]
    fn empty() {
        assert_eq!(santa(""), 0)
    }

    #[test]
    fn base1() {
        assert_eq!(santa("("), 1);
        assert_eq!(santa(")"), -1);
    }

    proptest! {
        #[test]
        fn base2(x in r"[^)(]") {
            prop_assert_eq!(santa(&x.to_string()), 0);
        }
    }

    proptest! {
        #[test]
        fn step(s: String, x in r".") {
            prop_assert_eq!(santa(&format!("{}{}", s, x)), santa(&s) + santa(&x));
        }
    }

    // The spec will panic only if the final value is out of range.  In
    // particular, the spec will return 0 for the string
    // `r"\({isize::MAX+1}\){isize::MAX+1}"`, but `santa` will probably panic
    // when reading/summing the last `(`.
    pub fn santa_spec(dirs: &str) -> isize {
        use num_bigint::BigInt;
        use num_traits::cast::ToPrimitive;

        let ups: BigInt = BigInt::from(dirs.chars().filter(|c| *c == '(').count());
        let downs: BigInt  = BigInt::from(dirs.chars().filter(|c| *c == ')').count());

        (ups - downs).to_isize().expect("out of range")
    }

    proptest! {
        #[test]
        fn spec1(x: String) {
            prop_assert_eq!(santa(x.as_str()), santa_spec(x.as_str()));
        }

        #[test]
        fn spec2(x in r"[)(]*") {
            prop_assert_eq!(santa(x.as_str()), santa_spec(x.as_str()));
        }
    }
}

// part two of the puzzle
fn underground_santa(dirs: &str) -> Option<usize> {
    let mut sum = 0isize;
    for (i, x) in dirs.chars().map(santa_char).enumerate() {
        sum += x;
        if sum < 0 {
            return Some(i + 1);
        }
    }
    None
}

mod test_part2 {
    #[cfg(not(verify))]
    use proptest::prelude::*;
    #[cfg(verify)]
    use propverify::prelude::*;

    use super::{santa, underground_santa};

    #[test]
    fn empty() {
        assert_eq!(underground_santa(""), None) // 0 indicates failure
    }

    #[test]
    fn base1() {
        assert_eq!(underground_santa("("), None);
        assert_eq!(underground_santa(")"), Some(1));
    }

    proptest! {
        #[test]
        fn base2(x in r"[^)(]") {
            prop_assert_eq!(underground_santa(&x), None);
        }
    }

    proptest! {
        #[test]
        fn step(s: String, x in r".") {
            prop_assert_eq!(underground_santa(&format!("{}{}", s, x)), underground_santa(&s).or_else(|| {
                if x == ")" && santa(&s) == 0 {
                    Some(s.chars().count() + 1)
                } else {
                    None
                }
            }));
        }
    }

    proptest! {
        #[test]
        fn step1(s: String, x in r"[^)]") {
            prop_assert_eq!(underground_santa(&format!("{}{}", s, x)), underground_santa(&s));
        }
    }

    proptest! {
        #[test]
        fn step2(s in any::<String>().prop_filter("", |s| { underground_santa(&s).is_none() })) {
            if santa(&s) == 0 {
                prop_assert_eq!(underground_santa(&format!("{})", s)), Some(s.chars().count() + 1));
            } else {
                prop_assert_eq!(underground_santa(&format!("{})", s)), None);
            }
        }
    }

    fn underground_santa_spec(dirs: &str) -> Option<usize> {
        use super::test_part1::santa_spec;

        dirs.char_indices()
            // Compute 'santa' for each position in the string
            .map(|(i, c)| santa_spec(&format!("{}{}", &dirs[..i], c)))
            // Find the first position in the string that goes below 0
            .position(|s| s < 0)
            // Increment the position by 1
            .map(|x| x + 1)
    }

    proptest! {
        #[test]
        fn spec1(s: String) {
            prop_assert_eq!(underground_santa(&s), underground_santa_spec(&s));
        }

        #[test]
        fn spec2(s in r"[)(]*") {
            prop_assert_eq!(underground_santa(&s), underground_santa_spec(&s));
        }
    }
}
