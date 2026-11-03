//! Regression coverage for Git path quoting, section fallback and hunk boundaries.
//! Uses the parser directly; process-boundary coverage lives in tests/task_diff.rs.

use super::*;

fn assert_paths(patch: &str, expected: &[&str]) {
    assert_eq!(
        parse(patch),
        expected.iter().map(PathBuf::from).collect::<Vec<_>>()
    );
}

#[test]
fn preserves_spaces_and_ignores_trailing_tab_fields() {
    assert_paths(
        "diff --git a/contracts/My Token.sol b/contracts/My Token.sol\n\
         --- a/contracts/My Token.sol\told\n+++ b/contracts/My Token.sol\tnew\n",
        &["contracts/My Token.sol"],
    );
}

#[test]
fn rename_keeps_directories_named_like_diff_sides() {
    assert_paths(
        "diff --git a/a/old.rs b/b/new.rs\nrename from a/old.rs\nrename to b/new.rs\n",
        &["a/old.rs", "b/new.rs"],
    );
}

#[test]
fn rename_and_copy_keep_both_paths_in_order() {
    assert_paths(
        "diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n\
         diff --git a/new.rs b/copy.rs\ncopy from new.rs\ncopy to copy.rs\n",
        &["old.rs", "new.rs", "copy.rs"],
    );
}

#[test]
fn deleted_and_new_empty_files_are_touched() {
    assert_paths(
        "diff --git a/deleted.rs b/deleted.rs\n--- a/deleted.rs\n+++ /dev/null\n\
         diff --git a/empty.rs b/empty.rs\nnew file mode 100644\n",
        &["deleted.rs", "empty.rs"],
    );
}

#[test]
fn hunk_body_headers_are_not_paths() {
    assert_paths(
        "diff --git a/real.rs b/real.rs\n--- a/real.rs\n+++ b/real.rs\n\
         @@ -1 +1 @@\n--- a/removed-evil.rs\n+++ b/evil.rs\n\
         @@ -3,0 +3,1 @@\n+++ b/another-evil.rs\n",
        &["real.rs"],
    );
}

#[test]
fn minimal_hunk_disables_headers_until_next_section() {
    assert_paths(
        "diff --git a/a b/a\n+++ b/a\n@@\n+x\n@@ -1 +1 @@\n-x\n+y\n+++ b/evil.rs\n\
         diff --git a/next b/next\n+++ b/next\n",
        &["a", "next"],
    );
}

#[test]
fn mode_and_binary_fallback_preserve_spaced_and_quoted_names() {
    assert_paths(
        "diff --git a/a b/a b/a b/a\nold mode 100644\nnew mode 100755\n\
         diff --git \"a/caf\\303\\251.bin\" \"b/caf\\303\\251.bin\"\n\
         Binary files differ\n",
        &["a b/a", "café.bin"],
    );
}

#[test]
fn explicit_headers_replace_fallback_and_deduplicate_in_order() {
    assert_paths(
        "diff --git a/unused b/unused\n--- a/old\n+++ b/new\nrename from old\nrename to new\n",
        &["old", "new"],
    );
}

#[test]
fn decodes_git_byte_escapes_and_lossy_utf8() {
    assert_paths(
        r#"+++ "b/caf\303\251\t\n\r\a\b\f\v\"\\\377.rs""#,
        &["café\t\n\r\x07\x08\x0c\x0b\"\\�.rs"],
    );
}
