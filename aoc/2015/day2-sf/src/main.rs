use std::{error, fs, str::FromStr};

use present::Present;

#[cfg_attr(test, test)]
fn main() -> Result<(), Box<dyn error::Error>> {
    let presents: Vec<_> = fs::read_to_string("input.txt")?
        .lines()
        .map(Present::from_str)
        .collect::<Result<_, _>>()?;

    let wrap = wrap(&presents);
    let ribbon = ribbon(&presents);

    println!("wrap: {}", wrap);
    println!("ribbon: {}", ribbon);

    assert_eq!(wrap, 1586300);
    assert_eq!(ribbon, 3737498);

    Ok(())
}

fn wrap(presents: &[Present]) -> usize {
    presents.iter().map(Present::wrap).sum()
}

fn ribbon(presents: &[Present]) -> usize {
    presents.iter().map(Present::ribbon).sum()
}

mod present {
    use std::{
        cmp::{max, min},
        error, fmt,
        str::FromStr,
    };

    #[derive(Copy, Clone, PartialEq, Debug)]
    pub struct Present {
        pub l: usize,
        pub w: usize,
        pub h: usize,
    }

    impl Present {
        #[allow(dead_code)]
        pub fn new(l: usize, w: usize, h: usize) -> Self {
            Present { l, w, h }
        }

        pub fn wrap(&self) -> usize {
            2 * (self.l * self.w + self.w * self.h + self.h * self.l)
                + min(self.l * self.w, min(self.w * self.h, self.h * self.l))
        }

        pub fn ribbon(&self) -> usize {
            2 * (self.l + self.w + self.h - max(self.l, max(self.w, self.h)))
                + self.l * self.w * self.h
        }
    }

    impl fmt::Display for Present {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "{}x{}x{}", self.l, self.w, self.h)
        }
    }

    impl FromStr for Present {
        type Err = Box<dyn error::Error>;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let dims: Vec<usize> = s.split("x").map(str::parse).collect::<Result<_, _>>()?;
            if dims.len() != 3 {
                Err(DimensionsError)?;
            }
            return Ok(Present {
                l: dims[0],
                w: dims[1],
                h: dims[2],
            });
        }
    }

    #[derive(Debug)]
    struct DimensionsError;

    impl fmt::Display for DimensionsError {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "Present should have exactly three diminsions!")
        }
    }

    impl error::Error for DimensionsError {}
}

// We will verify the code above twice. First with a complete spec, which is an
// overkill in this case. Then, we will do it in a more concise way.

#[cfg(test)]
mod spec {
    use crate::present;
    use std::cmp::{max, min};

    // Abstraction of `Present`.
    #[derive(PartialEq, Debug)]
    pub struct Present {
        pub ds: (usize, usize, usize),
    }

    // Mapping from concrete to spec.
    impl From<present::Present> for Present {
        fn from(p: present::Present) -> Self {
            Present {
                ds: (p.l, p.w, p.h),
            }
        }
    }

    // Spec for the methods we want to check.
    impl Present {
        pub fn wrap(&self) -> usize {
            let (l, w, h) = self.ds;
            2 * l * w + 2 * w * h + 2 * h * l + min(l * w, min(w * h, h * l))
        }

        pub fn ribbon(&self) -> usize {
            let (l, w, h) = self.ds;
            2 * l + 2 * w + 2 * h - 2 * max(l, max(w, h)) + (l * w * h)
        }
    }

    // Recursive spec of `wrap`.
    pub fn wrap(ss: &[Present]) -> usize {
        match ss {
            [] => 0,
            [head, tail @ ..] => head.wrap() + wrap(tail),
        }
    }

    // Recursive spec of `ribbon`.
    pub fn ribbon(ss: &[Present]) -> usize {
        match ss {
            [] => 0,
            [head, tail @ ..] => head.ribbon() + ribbon(tail),
        }
    }
}

#[cfg(test)]
mod test_elaborated_spec {
    #[cfg(verify)]
    use propverify as proptest;

    use proptest::prelude::*;

    use crate::spec;
    use crate::{present, ribbon, wrap};
    use std::str::FromStr;

    // Limit the values to avoid overflows.
    const UMAX: usize = 100000;

    // Strategy for `present::Present`.
    prop_compose! {
        fn arb_present()
            (l in 0..UMAX,
             w in 0..UMAX,
             h in 0..UMAX) -> present::Present {
                present::Present::new(l, w, h)
            }
    }

    // Check parsing and printing.
    proptest! {
        #[test]
        fn check_present_parse(s in arb_present().prop_map(spec::Present::from)) {
            prop_assert_eq!(spec::Present::from(present::Present::from_str(&format!("{}x{}x{}", s.ds.0, s.ds.1, s.ds.2)).unwrap()), s)
        }

        #[test]
        fn check_present_print(p in arb_present()) {
            // In general printing and parsing back might result in a slightly
            // different concrete (from `p`), but it should be mapped to the
            // same abstraction.
            prop_assert_eq!(spec::Present::from(present::Present::from_str(&p.to_string()).unwrap()), spec::Present::from(p))
        }
    }

    // Check that `present::Present::wrap` matches its spec.
    proptest! {
        #[test]
        fn check_present_wrap(p in arb_present()) {
            prop_assert_eq!(p.wrap(), spec::Present::from(p).wrap());
        }
    }

    // Check that `wrap` matches its spec.
    proptest! {
        #[test]
        fn check_wrap(ps in proptest::collection::vec(arb_present(), 0..1000)) {
            let ss: Vec<_> = ps.iter().cloned().map(spec::Present::from).collect();
            prop_assert_eq!(wrap(&ps), spec::wrap(&ss));
        }
    }

    // Check that `present::Present::ribbon` matches its spec.
    proptest! {
        #[test]
        fn present_ribbon_to_spec(p in arb_present()) {
            prop_assert_eq!(p.ribbon(), spec::Present::from(p).ribbon());
        }
    }

    // Check that `ribbon` matches its spec.
    proptest! {
        #[test]
        fn ribbon_to_spec(ps in proptest::collection::vec(arb_present(), 0..1000)) {
            let ss: Vec<_> = ps.iter().cloned().map(spec::Present::from).collect();
            prop_assert_eq!(ribbon(&ps), spec::ribbon(&ss));
        }
    }
}

#[cfg(test)]
mod tests_concise {
    #[cfg(verify)]
    use propverify as proptest;

    use proptest::prelude::*;

    use crate::{present::Present, ribbon, wrap};
    use std::{
        cmp::{max, min},
        str::FromStr,
    };

    // Note that we are not using `spec`!  We only need spec for `Present::wrap`
    // and `Present::ribbon`, which follow below.
    fn present_wrap_spec(Present { l, w, h }: Present) -> usize {
        2 * l * w + 2 * w * h + 2 * h * l + min(l * w, min(w * h, h * l))
    }

    fn present_ribbon_spec(Present { l, w, h }: Present) -> usize {
        (2 * (l + w + h - max(l, max(w, h)))) + (l * w * h)
    }

    // Limit the values to avoid overflows.
    const UMAX: usize = 100000;

    // Strategy for `Present`.
    prop_compose! {
        fn arb_present()
            (l in 0..UMAX,
             w in 0..UMAX,
             h in 0..UMAX) -> Present {
                Present::new(l, w, h)
            }
    }

    // Test parsing and printing.
    // Because we don't do fancy parsing, or printing, printing and parsing is
    // stable so we can do simple round-trips to check them.
    proptest! {
        #[test]
        fn check_present_parse(s in r"(0|[1-9][0-9]{0,4})x(0|[1-9][0-9]{0,4})x(0|[1-9][0-9]{0,4})") {
            prop_assert_eq!(&Present::from_str(&s).unwrap().to_string(), &s)
        }

        #[test]
        fn check_present_print(p in arb_present()) {
            prop_assert_eq!(Present::from_str(&p.to_string()).unwrap(), p)
        }
    }

    // Instead of checking `wrap` against `spec::wrap`, we will check it against
    // the two match cases in `spec::wrap`.
    // NOTE: This is not an inductive proof that `wrap` matches some spec. It's
    // just a way to avoid writing the spec explicitly.

    // First case, empty `Vec`, should return 0.
    #[test]
    fn check_wrap_case1() {
        assert_eq!(wrap(&vec![]), 0);
    }

    // Then, we will gradually increase the size of the `Vec`, assuming smaller
    // vecs give the correct result.
    proptest! {
        #[test]
        fn check_wrap_case2(tail in proptest::collection::vec(arb_present(), 0..1000),
                            head in arb_present()) {
            // Assuming we have already checked wrap for vecs of size
            // `tail.len()`, `wrap(&tail)` is equivalent to its spec. Hence we
            // can use it in `spec` for the concat below.
            let spec = present_wrap_spec(head) + wrap(&tail);
            prop_assert_eq!(wrap(&[vec![head], tail].concat()), spec);
        }
    }

    #[test]
    fn ribbon_case1() {
        assert_eq!(ribbon(&vec![]), 0usize);
    }

    proptest! {
        #[test]
        fn ribbon_case2(tail in proptest::collection::vec(arb_present(), 0..1000),
                        head in arb_present()) {
            let spec = present_ribbon_spec(head) + ribbon(&tail);
            prop_assert_eq!(ribbon(&[vec![head], tail].concat()), spec);
        }
    }
}
