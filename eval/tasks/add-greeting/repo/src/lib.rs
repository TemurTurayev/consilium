// TASK: add a public function `greeting() -> String` that returns "hello".
// The test below already asserts this and must pass unchanged.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_says_hello() {
        assert_eq!(greeting(), "hello");
    }
}
