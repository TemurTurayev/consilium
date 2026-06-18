// Protected oracle — restored from the baseline commit before scoring.
#[test]
fn sums_inclusive() {
    assert_eq!(fix_sum::sum_to(5), 15);
    assert_eq!(fix_sum::sum_to(1), 1);
    assert_eq!(fix_sum::sum_to(0), 0);
}
