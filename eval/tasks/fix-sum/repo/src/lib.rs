// BUG: `sum_to(n)` should return the sum of 1..=n (inclusive), but it stops at
// n - 1. The test in tests/sum.rs catches this — fix the function here.
pub fn sum_to(n: u32) -> u32 {
    (1..n).sum()
}
