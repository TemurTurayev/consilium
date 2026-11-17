// Protected oracle — restored from the baseline commit before scoring.
use path_extension::extension;

fn ext(path: &str) -> Option<String> {
    extension(path)
}

#[test]
fn returns_the_final_extension() {
    assert_eq!(ext("notes.txt"), Some("txt".to_string()));
    assert_eq!(ext("archive.tar.gz"), Some("gz".to_string()));
    assert_eq!(ext("/var/log/app.log"), Some("log".to_string()));
}

#[test]
fn lowercases_the_extension() {
    assert_eq!(ext("report.PDF"), Some("pdf".to_string()));
    assert_eq!(ext("a/b/photo.JPeG"), Some("jpeg".to_string()));
}

#[test]
fn a_name_without_a_dot_has_no_extension() {
    assert_eq!(ext("README"), None);
    assert_eq!(ext("bin/consilium"), None);
    assert_eq!(ext(""), None);
}

#[test]
fn a_dot_in_a_directory_name_is_not_an_extension() {
    assert_eq!(ext("releases.d/notes"), None);
    assert_eq!(ext("v1.2/readme"), None);
}

#[test]
fn a_dotfile_has_no_extension() {
    assert_eq!(ext(".bashrc"), None);
    assert_eq!(ext("/home/ada/.gitignore"), None);
}

#[test]
fn a_dotfile_can_still_carry_an_extension() {
    assert_eq!(ext(".config.toml"), Some("toml".to_string()));
}

#[test]
fn a_trailing_dot_is_not_an_extension() {
    assert_eq!(ext("notes."), None);
    assert_eq!(ext("dir/"), None);
}
