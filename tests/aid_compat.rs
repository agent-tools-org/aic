//! Judge CLI compatibility at the subprocess boundary.
//! Uses isolated fake aid executables and real L2/dispatcher calls.
#![cfg(unix)]
use aic::config::{L2, L3};
use aic::layers::aid_dispatch::{AidDispatcher, RealAidDispatcher};
use aic::layers::{l2::L2Runner, l3::L3Runner};
use aic::Verdict;
use std::{error::Error, fs, os::unix::fs::PermissionsExt, path::Path};
use tempfile::tempdir;

const FAKE_AID: &str = r#"#!/bin/sh
if [ "$1" = show ]; then
    cat "$RECORD/answer"
    exit
fi
[ "$1" = run ] && [ "$4" = --context ] || exit 8
printf '%s\n' "$@" > "$RECORD/args"
printf '%s' "$5" > "$RECORD/path"
cat "$5" > "$RECORD/prompt" || exit 9
if [ "$FAIL_JUDGE" = 1 ]; then exit 7; fi
printf 'Task t-abc started (judge)\nTask t-abc DONE (1s, 100 tokens, $0.01)\n'
"#;

#[test]
fn judge_process_contract() -> Result<(), Box<dyn Error>> {
    if let Ok(root) = std::env::var("JUDGE_CHILD") {
        return child_calls(Path::new(&root));
    }
    let dir = tempdir()?;
    fs::write(
        dir.path().join("answer"),
        r#"{"verdict":"pass","reason":"ok","cited_lines":[]}"#,
    )?;
    let aid = dir.path().join("aid");
    fs::write(&aid, FAKE_AID)?;
    fs::set_permissions(&aid, fs::Permissions::from_mode(0o755))?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "judge_process_contract", "--nocapture"])
        .env("JUDGE_CHILD", dir.path())
        .env("RECORD", dir.path())
        .env(
            "PATH",
            format!("{}:{}", dir.path().display(), std::env::var("PATH")?),
        )
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn child_calls(root: &Path) -> Result<(), Box<dyn Error>> {
    let prompt = "large diff\n".repeat(20_000);
    RealAidDispatcher.dispatch("custom-agent", &prompt)?;
    assert_eq!(fs::read_to_string(root.join("prompt"))?, prompt);
    let path = fs::read_to_string(root.join("path"))?;
    assert!(!Path::new(&path).exists());
    let expected = format!(
        "run\ncustom-agent\n\
         Follow the instructions in the attached context file and return only the output they specify.\n\
         --context\n{path}\n--read-only\n--no-audit\n--verify\nnone\n\
         --difficulty\nsimple\n--budget\nstandard\n--urgency\nnormal\n--rigor\nstandard\n"
    );
    assert_eq!(fs::read_to_string(root.join("args"))?, expected);
    std::env::set_var("FAIL_JUDGE", "1");
    assert!(RealAidDispatcher.dispatch("custom-agent", &prompt).is_err());
    let path = fs::read_to_string(root.join("path"))?;
    assert!(!Path::new(&path).exists());
    std::env::remove_var("FAIL_JUDGE");
    fs::write(
        root.join("rule.md"),
        "---\nid: custom\nseverity: blocking\nscope: ['src/**']\nlayer: l2\n---\n# Check\nPass.",
    )?;
    let config = L2 {
        model: "configured-agent".into(),
        ..L2::default()
    };
    L2Runner::new(&config, root.to_owned()).run("diff", &["src/lib.rs".into()], &[]);
    assert!(fs::read_to_string(root.join("args"))?.starts_with("run\nconfigured-agent\n"));
    check_final_answers(root)?;
    Ok(())
}

#[test]
fn missing_aid_cleans_prompt_file() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    fs::create_dir(dir.path().join("tmp"))?;
    fs::write(
        dir.path().join("diff.patch"),
        "diff --git a/a b/a\n+++ b/a\n@@\n+x\n",
    )?;
    fs::create_dir(dir.path().join(".aic"))?;
    fs::write(
        dir.path().join(".aic/config.toml"),
        "[layers.l0]\nsecret_scan=false\naddress_verify=false\n[layers.l2]\nenabled=false\n",
    )?;
    assert_cmd::Command::cargo_bin("aic")?
        .current_dir(dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("isolated-config"))
        .env("PATH", dir.path())
        .env("TMPDIR", dir.path().join("tmp"))
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .code(3);
    assert_eq!(fs::read_dir(dir.path().join("tmp"))?.count(), 0);
    Ok(())
}

fn check_final_answers(root: &Path) -> Result<(), Box<dyn Error>> {
    let l2 = L2 {
        model: "configured-agent".into(),
        ..L2::default()
    };
    let l3 = L3 {
        models: vec!["configured-agent".into()],
        swap: false,
        ..L3::default()
    };
    let pass = r#"{"verdict":"pass","reason":"No unwrap usage.","cited_lines":[]}"#;
    let fail = r#"{"verdict":"fail","reason":"Blocking bug","bugs":[{"impact":"high","bug":"bug","trigger":"any input"}]}"#;
    let preamble = "I'll read the full context file and keep all actions read-only.\n\n---\n\
        [MILESTONE] Read the context and checked the diff for unwrap usage.\n\n---\n";
    let cases = [
        (
            format!("## Findings\n- Blocking bug. Fixture: {pass}"),
            "inconclusive",
        ),
        (format!("{preamble}{pass}"), "pass"),
        (format!("{pass}\n---\n{preamble}{fail}"), "fail"),
        (
            format!("{fail}\n---\n{preamble}```json\n{pass}\n```"),
            "pass",
        ),
        (format!("{pass} trailing prose"), "inconclusive"),
        (format!("{pass}\n{fail}"), "inconclusive"),
        (format!("{pass}\n---\nNo final JSON"), "inconclusive"),
    ];
    for (output, expected) in cases {
        fs::write(root.join("answer"), &output)?;
        let (l2_result, _) =
            L2Runner::new(&l2, root.to_owned()).run("diff", &["src/lib.rs".into()], &[]);
        let (l3_result, _) = L3Runner::new(&l3).run("diff", None, &[], &[]);
        for result in [l2_result, l3_result] {
            match expected {
                "pass" => assert!(matches!(result.verdict, Verdict::Pass), "{output}"),
                "fail" => assert!(
                    matches!(result.verdict, Verdict::FailBlocking { .. }),
                    "{output}"
                ),
                _ => assert!(
                    matches!(result.verdict, Verdict::Inconclusive { .. }),
                    "{output}"
                ),
            }
        }
        let context = fs::read_to_string(root.join("prompt"))?;
        assert!(context.contains("Do NOT read any other file"));
        assert!(context.contains("or make any other tool call"));
        let path = fs::read_to_string(root.join("path"))?;
        assert!(!Path::new(&path).exists());
    }
    Ok(())
}

#[test]
fn oversized_diff_is_inconclusive_without_calling_a_judge() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    let aid = dir.path().join("aid");
    fs::write(&aid, "#!/bin/sh\ntouch \"$RECORD/called\"\n")?;
    fs::set_permissions(&aid, fs::Permissions::from_mode(0o755))?;
    let lines = "+filler line\n".repeat(21_000);
    fs::write(dir.path().join("big.patch"), format!("diff --git a/a b/a\n+++ b/a\n@@\n{lines}"))?;
    let path = format!("{}:{}", dir.path().display(), std::env::var("PATH")?);
    let output = assert_cmd::Command::cargo_bin("aic")?
        .current_dir(dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("isolated-config"))
        .env("PATH", path)
        .env("RECORD", dir.path())
        .args(["audit", "--diff", "big.patch"])
        .assert()
        .code(3)
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(output)?.contains("byte judge limit"));
    assert!(!dir.path().join("called").exists());
    Ok(())
}
