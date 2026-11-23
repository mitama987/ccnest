use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 開き直す既存 Claude セッション (`--resume <uuid>[=<タブ名>]`)。
/// 起動時の引数と、動いている窓への依頼 (`remote::Request`) の両方で使う。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeSpec {
    pub session_id: Uuid,
    /// タブ名。None ならフォルダ名 (新規タブと同じ)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 再開してすぐ送る最初の一言 (`--resume-prompt-file`)。None なら開くだけ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// このセッションだけの開始モード (`--resume-permission-mode`)。None なら
    /// ほかのタブと同じ (`CCNEST_CLAUDE_PERMISSION_MODE`・既定 plan)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// このセッションだけのモデル (`--resume-model`)。None なら claude の既定
    /// (再開するセッションが前に使っていたモデル)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// コマンドライン引数の解析結果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cli {
    /// 最初のペインの cwd。None なら current_dir。
    pub cwd: Option<PathBuf>,
    /// 1 セッション 1 タブで開き直すセッション。空なら従来どおり新規 1 ペイン。
    pub resumes: Vec<ResumeSpec>,
    /// `--add`: 新しい窓を開かず、動いている ccnest の窓へ `resumes` を送る。
    pub add: bool,
    /// `--to <pid>`: `--add` の宛先。None なら環境変数 `CCNEST_PID`。
    pub to: Option<u32>,
    /// `--close`: 新しい窓を開かず、このコマンドを打ったペインのあるタブを、動いている
    /// 窓に閉じてもらう (宛先は環境変数 `CCNEST_PID` / `CCNEST_PANE`)。
    pub close: bool,
    /// `--after <秒>`: `--close` が実際に閉じるまでの秒数。None なら
    /// [`DEFAULT_CLOSE_AFTER_SECS`]。
    pub after: Option<u64>,
}

/// `--close` の既定の待ち秒数 (最後の返事を画面に出す時間)。
pub const DEFAULT_CLOSE_AFTER_SECS: u64 = 5;
/// `--after` の上限。
pub const MAX_CLOSE_AFTER_SECS: u64 = 600;

/// `ccnest [cwd] [--resume <uuid>[=<タブ名>]]...` と
/// `ccnest --add [--to <pid>] --resume <uuid>[=<タブ名>]...` を解析する
/// (`args` はプログラム名を除いたもの)。`--resume-prompt-file` のファイルはここで読む。
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Cli> {
    parse_args_with(args, |path| {
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))
    })
}

/// [`parse_args`] の本体。ファイルの読み取りを注入するので純粋にテストできる。
///
/// `--resume` の後ろに付けられる指定 (どちらも直前の `--resume` 1 つにだけ効く):
/// - `--resume-prompt-file <path>`: 再開してすぐ、ファイルの文面を最初の一言として
///   送る (再起動で止まった作業の続きを指示する用)。文面は 1 行に畳む。
/// - `--resume-permission-mode <mode>`: そのセッションだけこのモードで始める
///   (`bypassPermissions` / `plan` / `acceptEdits` …)。
/// - `--resume-model <model>`: そのセッションだけこのモデルで始める
///   (`claude --model <model>`・`fable` / `opus` / `claude-fable-5-1` …)。
pub fn parse_args_with<I, F>(args: I, read_file: F) -> Result<Cli>
where
    I: IntoIterator<Item = String>,
    F: Fn(&Path) -> Result<String>,
{
    let mut cli = Cli::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        if arg == "--resume" {
            let value = it
                .next()
                .ok_or_else(|| anyhow!("--resume needs <uuid>[=<title>]"))?;
            cli.resumes.push(parse_resume(&value)?);
        } else if let Some(value) = arg.strip_prefix("--resume=") {
            cli.resumes.push(parse_resume(value)?);
        } else if arg == "--resume-prompt-file" {
            let value = it
                .next()
                .ok_or_else(|| anyhow!("--resume-prompt-file needs <path>"))?;
            set_prompt(&mut cli, &value, &read_file)?;
        } else if let Some(value) = arg.strip_prefix("--resume-prompt-file=") {
            set_prompt(&mut cli, value, &read_file)?;
        } else if arg == "--resume-permission-mode" {
            let value = it
                .next()
                .ok_or_else(|| anyhow!("--resume-permission-mode needs <mode>"))?;
            set_permission_mode(&mut cli, &value)?;
        } else if let Some(value) = arg.strip_prefix("--resume-permission-mode=") {
            set_permission_mode(&mut cli, value)?;
        } else if arg == "--resume-model" {
            let value = it
                .next()
                .ok_or_else(|| anyhow!("--resume-model needs <model>"))?;
            set_model(&mut cli, &value)?;
        } else if let Some(value) = arg.strip_prefix("--resume-model=") {
            set_model(&mut cli, value)?;
        } else if arg == "--add" {
            cli.add = true;
        } else if arg == "--to" {
            let value = it.next().ok_or_else(|| anyhow!("--to needs <pid>"))?;
            cli.to = Some(parse_pid(&value)?);
        } else if let Some(value) = arg.strip_prefix("--to=") {
            cli.to = Some(parse_pid(value)?);
        } else if arg == "--close" {
            cli.close = true;
        } else if arg == "--after" {
            let value = it
                .next()
                .ok_or_else(|| anyhow!("--after needs <seconds>"))?;
            cli.after = Some(parse_after(&value)?);
        } else if let Some(value) = arg.strip_prefix("--after=") {
            cli.after = Some(parse_after(value)?);
        } else if arg.starts_with("--") {
            return Err(anyhow!("unknown option: {arg}"));
        } else if cli.cwd.is_none() {
            cli.cwd = Some(PathBuf::from(arg));
        } else {
            return Err(anyhow!("unexpected argument: {arg}"));
        }
    }
    if cli.close {
        if cli.add || !cli.resumes.is_empty() || cli.to.is_some() || cli.cwd.is_some() {
            return Err(anyhow!(
                "--close cannot be combined with --add / --resume / --to / a folder (it closes the tab of the pane it is typed in)"
            ));
        }
        return Ok(cli);
    }
    if cli.after.is_some() {
        return Err(anyhow!("--after is only valid with --close"));
    }
    if cli.add {
        if cli.resumes.is_empty() {
            return Err(anyhow!(
                "--add needs at least one --resume <uuid>[=<title>]"
            ));
        }
        if cli.cwd.is_some() {
            return Err(anyhow!(
                "--add takes no folder (tabs open in the running window's folder)"
            ));
        }
    } else if cli.to.is_some() {
        return Err(anyhow!("--to is only valid with --add"));
    }
    Ok(cli)
}

fn parse_after(value: &str) -> Result<u64> {
    let secs = value
        .trim()
        .parse::<u64>()
        .map_err(|e| anyhow!("--after: invalid seconds {value:?}: {e}"))?;
    if secs > MAX_CLOSE_AFTER_SECS {
        return Err(anyhow!(
            "--after: {secs} is too long (max {MAX_CLOSE_AFTER_SECS} seconds)"
        ));
    }
    Ok(secs)
}

fn parse_pid(value: &str) -> Result<u32> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|e| anyhow!("--to: invalid pid {value:?}: {e}"))
}

fn parse_resume(value: &str) -> Result<ResumeSpec> {
    let (id, title) = match value.split_once('=') {
        Some((id, title)) => (id, Some(title.trim())),
        None => (value, None),
    };
    let session_id = Uuid::parse_str(id.trim())
        .map_err(|e| anyhow!("--resume: invalid session id {id:?}: {e}"))?;
    Ok(ResumeSpec {
        session_id,
        title: title.filter(|t| !t.is_empty()).map(str::to_string),
        prompt: None,
        permission_mode: None,
        model: None,
    })
}

fn last_resume<'a>(cli: &'a mut Cli, option: &str) -> Result<&'a mut ResumeSpec> {
    cli.resumes
        .last_mut()
        .ok_or_else(|| anyhow!("{option} must come after a --resume"))
}

fn set_prompt(
    cli: &mut Cli,
    path: &str,
    read_file: &impl Fn(&Path) -> Result<String>,
) -> Result<()> {
    let spec = last_resume(cli, "--resume-prompt-file")?;
    let raw = read_file(Path::new(path))?;
    let prompt = normalize_prompt(&raw)
        .ok_or_else(|| anyhow!("--resume-prompt-file: {path} has no text"))?;
    spec.prompt = Some(prompt);
    Ok(())
}

fn set_model(cli: &mut Cli, model: &str) -> Result<()> {
    let spec = last_resume(cli, "--resume-model")?;
    let model = model.trim();
    // モデル名 (別名 `fable` / 完全名 `claude-fable-5-1` / `opus[1m]`)。`-` 始まりは
    // claude がオプションと読むので不可。
    let ok = !model.is_empty()
        && !model.starts_with('-')
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']' | ':'));
    if !ok {
        return Err(anyhow!("--resume-model: invalid model {model:?}"));
    }
    spec.model = Some(model.to_string());
    Ok(())
}

fn set_permission_mode(cli: &mut Cli, mode: &str) -> Result<()> {
    let spec = last_resume(cli, "--resume-permission-mode")?;
    let mode = mode.trim();
    if mode.is_empty() || !mode.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(anyhow!("--resume-permission-mode: invalid mode {mode:?}"));
    }
    spec.permission_mode = Some(mode.to_string());
    Ok(())
}

/// 純粋: ファイルの文面を claude の引数 1 つにする。BOM を外し、改行と連続する
/// 空白を 1 つの空白に畳む (引数に改行が入るとシム越しの受け渡しで壊れうる)。
/// 中身が無ければ None。`-` で始まる文面は claude がオプションと読むので None
/// (文面は平文だけを想定)。
pub fn normalize_prompt(raw: &str) -> Option<String> {
    let text = raw
        .trim_start_matches('\u{feff}')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty() && !text.starts_with('-')).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "7f460491-997d-48bd-9118-497e59f4bc95";
    const ID2: &str = "d86c5a88-97f6-4d96-a6c1-5433ec41d932";

    fn parse(args: &[&str]) -> Result<Cli> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args_is_default() {
        assert_eq!(parse(&[]).unwrap(), Cli::default());
    }

    #[test]
    fn cwd_only_keeps_legacy_behavior() {
        let cli = parse(&[r"C:\work\repo"]).unwrap();
        assert_eq!(cli.cwd, Some(PathBuf::from(r"C:\work\repo")));
        assert!(cli.resumes.is_empty());
    }

    #[test]
    fn repeated_resume_with_and_without_title() {
        let with_title = format!("{ID}=discord-open-issues-slides (bg)");
        let cli = parse(&[r"C:\work\repo", "--resume", &with_title, "--resume", ID2]).unwrap();
        assert_eq!(cli.cwd, Some(PathBuf::from(r"C:\work\repo")));
        assert_eq!(
            cli.resumes,
            [
                ResumeSpec {
                    session_id: Uuid::parse_str(ID).unwrap(),
                    title: Some("discord-open-issues-slides (bg)".to_string()),
                    prompt: None,
                    permission_mode: None,
                    model: None,
                },
                ResumeSpec {
                    session_id: Uuid::parse_str(ID2).unwrap(),
                    title: None,
                    prompt: None,
                    permission_mode: None,
                    model: None,
                },
            ]
        );
    }

    #[test]
    fn resume_equals_form_and_cwd_after_options() {
        let arg = format!("--resume={ID}=name");
        let cli = parse(&[&arg, r"C:\work\repo"]).unwrap();
        assert_eq!(cli.cwd, Some(PathBuf::from(r"C:\work\repo")));
        assert_eq!(cli.resumes[0].title.as_deref(), Some("name"));
    }

    #[test]
    fn empty_title_means_folder_name() {
        let arg = format!("{ID}=");
        let cli = parse(&["--resume", &arg]).unwrap();
        assert_eq!(cli.resumes[0].title, None);
    }

    #[test]
    fn errors_on_bad_input() {
        assert!(parse(&["--resume"]).is_err());
        assert!(parse(&["--resume", "not-a-uuid"]).is_err());
        assert!(parse(&["--help"]).is_err());
        assert!(parse(&["a", "b"]).is_err());
    }

    /// `--add`: 動いている窓へ送る。`--to <pid>` は宛先 (無ければ CCNEST_PID)。
    #[test]
    fn add_with_and_without_to() {
        let cli = parse(&["--add", "--resume", ID]).unwrap();
        assert!(cli.add);
        assert_eq!(cli.to, None);
        assert_eq!(cli.resumes.len(), 1);

        let cli = parse(&["--add", "--to", "15416", "--resume", ID, "--resume", ID2]).unwrap();
        assert_eq!(cli.to, Some(15416));
        assert_eq!(cli.resumes.len(), 2);

        let cli = parse(&["--resume", ID, "--to=7", "--add"]).unwrap();
        assert!(cli.add, "順番は問わない");
        assert_eq!(cli.to, Some(7));
    }

    /// `--add` の誤用は raw mode に入る前にエラーで終わる。
    #[test]
    fn add_errors() {
        assert!(parse(&["--add"]).is_err(), "--resume が 1 つも無い");
        assert!(parse(&["--to", "1", "--resume", ID]).is_err(), "--to だけ");
        assert!(parse(&["--add", "--to"]).is_err());
        assert!(parse(&["--add", "--to", "abc", "--resume", ID]).is_err());
        assert!(
            parse(&["--add", r"C:\work\repo", "--resume", ID]).is_err(),
            "フォルダは窓側のものを使うので指定できない"
        );
    }

    fn parse_with_file(args: &[&str], content: &'static str) -> Result<Cli> {
        parse_args_with(args.iter().map(|s| s.to_string()), move |_| {
            Ok(content.to_string())
        })
    }

    /// `--resume-prompt-file` / `--resume-permission-mode` は直前の `--resume`
    /// 1 つにだけ効く (途中だったセッションだけ続きを指示する)。
    #[test]
    fn resume_prompt_and_mode_apply_to_the_preceding_resume_only() {
        let cli = parse_with_file(
            &[
                r"C:\work\repo",
                "--resume",
                ID,
                "--resume-prompt-file",
                r"C:\x\prompt.txt",
                "--resume-permission-mode",
                "bypassPermissions",
                "--resume",
                ID2,
            ],
            "\u{feff}続きを進めて。\r\n判断待ちは  進めない。\n",
        )
        .unwrap();
        assert_eq!(
            cli.resumes[0].prompt.as_deref(),
            Some("続きを進めて。 判断待ちは 進めない。"),
            "BOM を外して 1 行に畳む"
        );
        assert_eq!(
            cli.resumes[0].permission_mode.as_deref(),
            Some("bypassPermissions")
        );
        assert_eq!(
            cli.resumes[1].prompt, None,
            "指定していないセッションは開くだけ"
        );
        assert_eq!(cli.resumes[1].permission_mode, None);
    }

    #[test]
    fn resume_prompt_equals_forms() {
        let cli = parse_with_file(
            &[
                "--resume",
                ID,
                "--resume-prompt-file=p.txt",
                "--resume-permission-mode=plan",
            ],
            "go on",
        )
        .unwrap();
        assert_eq!(cli.resumes[0].prompt.as_deref(), Some("go on"));
        assert_eq!(cli.resumes[0].permission_mode.as_deref(), Some("plan"));
    }

    #[test]
    fn resume_prompt_errors() {
        assert!(
            parse_with_file(&["--resume-prompt-file", "p.txt", "--resume", ID], "x").is_err(),
            "--resume より前には置けない"
        );
        assert!(
            parse_with_file(&["--resume", ID, "--resume-prompt-file", "p.txt"], " \n ").is_err(),
            "空のファイル"
        );
        assert!(
            parse_with_file(&["--resume", ID, "--resume-prompt-file", "p.txt"], "--help").is_err(),
            "オプションに見える文面"
        );
        assert!(parse_with_file(&["--resume", ID, "--resume-prompt-file"], "x").is_err());
        assert!(parse_with_file(&["--resume-permission-mode", "plan"], "x").is_err());
        assert!(
            parse_with_file(&["--resume", ID, "--resume-permission-mode", "--plan"], "x").is_err(),
            "モードは英字だけ"
        );
        let unreadable = parse_args_with(
            ["--resume", ID, "--resume-prompt-file", "nope.txt"]
                .iter()
                .map(|s| s.to_string()),
            |_| Err(anyhow!("cannot read")),
        );
        assert!(unreadable.is_err(), "読めないファイルは起動前にエラー");
    }

    #[test]
    fn normalize_prompt_folds_to_one_line() {
        assert_eq!(normalize_prompt("a\r\n b\tc\n"), Some("a b c".to_string()));
        assert_eq!(normalize_prompt("\u{feff} \n"), None);
    }

    /// `--close [--after <秒>]`: 打ったペインのタブを閉じてもらう。
    #[test]
    fn close_with_and_without_after() {
        let cli = parse(&["--close"]).unwrap();
        assert!(cli.close);
        assert_eq!(cli.after, None);
        assert!(!cli.add);

        let cli = parse(&["--close", "--after", "10"]).unwrap();
        assert_eq!(cli.after, Some(10));
        let cli = parse(&["--after=0", "--close"]).unwrap();
        assert_eq!(cli.after, Some(0), "順番は問わない・0 は許す");
        assert_eq!(
            parse(&["--close", "--after", "600"]).unwrap().after,
            Some(600)
        );
    }

    /// `--close` の誤用は raw mode に入る前にエラーで終わる。
    #[test]
    fn close_errors() {
        assert!(parse(&["--after", "5"]).is_err(), "--after だけ");
        assert!(parse(&["--close", "--after"]).is_err());
        assert!(parse(&["--close", "--after", "abc"]).is_err());
        assert!(parse(&["--close", "--after", "-1"]).is_err());
        assert!(parse(&["--close", "--after", "601"]).is_err(), "上限超え");
        assert!(parse(&["--close", "--add", "--resume", ID]).is_err());
        assert!(parse(&["--close", "--resume", ID]).is_err());
        assert!(parse(&["--close", "--to", "5"]).is_err());
        assert!(parse(&["--close", r"C:\work\repo"]).is_err());
        assert!(parse(&["--after", "5", "--resume", ID]).is_err());
    }

    /// `--resume-model` は直前の `--resume` 1 つにだけ効く (`--add` でも使える)。
    #[test]
    fn resume_model_applies_to_the_preceding_resume_only() {
        let cli = parse(&[
            "--add",
            "--resume",
            ID,
            "--resume-model",
            "fable",
            "--resume",
            ID2,
            "--resume-model=claude-fable-5-1",
        ])
        .unwrap();
        assert_eq!(cli.resumes[0].model.as_deref(), Some("fable"));
        assert_eq!(cli.resumes[1].model.as_deref(), Some("claude-fable-5-1"));
        let cli = parse(&[
            "--resume",
            ID,
            "--resume",
            ID2,
            "--resume-model",
            "opus[1m]",
        ])
        .unwrap();
        assert_eq!(
            cli.resumes[0].model, None,
            "指定していないセッションは既定のまま"
        );
        assert_eq!(cli.resumes[1].model.as_deref(), Some("opus[1m]"));
    }

    #[test]
    fn resume_model_errors() {
        assert!(
            parse(&["--resume-model", "fable", "--resume", ID]).is_err(),
            "--resume より前"
        );
        assert!(parse(&["--resume", ID, "--resume-model"]).is_err());
        assert!(parse(&["--resume", ID, "--resume-model", ""]).is_err());
        assert!(parse(&["--resume", ID, "--resume-model", "--fable"]).is_err());
        assert!(parse(&["--resume", ID, "--resume-model", "a b"]).is_err());
        assert!(parse(&["--resume", ID, "--resume-model", "a;b"]).is_err());
    }

    /// 既存の起動 (--add 無し) は add=false のまま。
    #[test]
    fn plain_launch_is_not_add() {
        let cli = parse(&[r"C:\work\repo", "--resume", ID]).unwrap();
        assert!(!cli.add);
        assert_eq!(cli.to, None);
    }
}

// Version History
// ver0.1 - 2026-10-19 - First version: positional cwd + repeated
//                       `--resume <uuid>[=<title>]` (reopen saved sessions,
//                       one tab each).
// ver0.2 - 2026-11-11 - `--add [--to <pid>]`: send the `--resume` sessions to
//                       a running ccnest window instead of starting one
//                       (see remote.rs). ResumeSpec derives Serialize /
//                       Deserialize for the request file.
// ver0.3 - 2026-11-21 - `--resume-prompt-file <path>` / `--resume-permission-mode
//                       <mode>` for the preceding `--resume` (ResumeSpec.prompt /
//                       .permission_mode): reopen a session that a reboot cut
//                       off, tell it to continue, in the mode it was in.
//                       parse_args_with injects the file read for tests.
// ver0.4 - 2026-11-23 - `--close [--after <seconds>]` (Cli.close / Cli.after,
//                       default 5 s, max 600): ask the running window to close
//                       the tab of the pane the command was typed in. Cannot be
//                       combined with --add / --resume / --to / a folder.
// ver0.5 - 2026-11-23 - `--resume-model <model>` for the preceding `--resume`
//                       (ResumeSpec.model, passed as `claude --model <model>`).
