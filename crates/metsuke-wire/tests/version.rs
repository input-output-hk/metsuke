//! How a binary names itself. The three binaries each assert `--version`
//! answers `version_line`, which holds them to the function and not to its
//! shape; this is the shape.

/// `.github/workflows/release.yml` reads a published binary's version and the
/// commit it was built from out of this line, and refuses to publish when
/// either disagrees with the tag. So the shape is a contract with the release
/// and not only with whoever is reading the output, and it is parsed here the
/// way the workflow parses it.
#[test]
fn a_version_line_is_the_version_then_the_commit_in_parentheses() {
    let line = metsuke_wire::version_line("1.2.3");
    let (version, rev) = line
        .split_once(" (")
        .expect("a space and an open parenthesis separate the two");

    assert_eq!(version, "1.2.3");
    assert_eq!(rev, format!("{})", metsuke_wire::BUILD_REV));
}
