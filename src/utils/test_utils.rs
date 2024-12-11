macro_rules! test_case {
    ($fname:expr) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/", $fname)
    };
}
pub(crate) use test_case;
