fn main() {
    let dirs = "";
    println!("Santa {} = {}", dirs, santa(dirs));
}

// todo: first attempt was wrong: used a usize for result
fn santa(dirs: &str) -> isize {
    let mut count = 0;
    for c in dirs.chars() {
        if c == '(' {
            count += 1;
        } else if c == ')' {
            count -= 1;
        } else {
            // first attempt: panic!("Malformed string");
        }
    }
    count
}

mod test {
    #[cfg(not(verify))]
    use proptest::prelude::*;
    #[cfg(verify)]
    use propverify::prelude::*;

    use super::santa;

    use std::convert::TryFrom;

    #[test]
    fn empty() {
        assert_eq!(santa(""), 0)
    }

    fn santa_onechar(x: char) -> isize {
        if x == '(' {
            1
        } else if x == ')' {
            -1
        } else {
            0
        }
    }

    proptest! {
        #[test]
        fn singleton(x: char) {
            let r = santa_onechar(x);
            prop_assert_eq!(santa(&x.to_string()), r); // nope
        }
    }

    proptest! {
        #[test]
        fn append(x: String, y: String) {
            prop_assert_eq!(santa(x.as_str()) + santa(y.as_str()), santa((x + &y).as_str()))
        }
    }

    proptest! {
        #[test]
        fn up(x in r"\(*") {
            let r = isize::try_from(x.len())?;
            prop_assert_eq!(santa(x.as_str()), r)
        }
    }

    proptest! {
        #[test]
        fn down(x in r"\)*") {
            let r = isize::try_from(x.len())?;
            prop_assert_eq!(santa(x.as_str()), -r)
        }
    }

    proptest! {
        #[test]
        fn none(x in r"[^()]*") {
            prop_assert_eq!(santa(x.as_str()), 0)
        }
    }
}