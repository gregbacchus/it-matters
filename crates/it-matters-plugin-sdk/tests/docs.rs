//! The SDK guide's example is a real, compiled example (`examples/lamp.rs`): the code on the page must be
//! exactly that file, so the documentation cannot drift from code that compiles.

#[test]
fn the_sdk_guide_shows_the_compiled_example() {
    let page = include_str!("../../../docs/plugins/sdk.md");
    let example = include_str!("../examples/lamp.rs");
    let block = page
        .split("```rust\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("docs/plugins/sdk.md has a rust code block");
    assert_eq!(block, example, "docs/plugins/sdk.md and crates/it-matters-plugin-sdk/examples/lamp.rs differ");
}
