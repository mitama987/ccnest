use std::path::PathBuf;

use anyhow::{anyhow, Result};
use uuid::Uuid;

/// 起動時に開き直す既存 Claude セッション (`--resume <uuid>[=<タブ名>]`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeSpec {
    pub session_id: Uuid,
    /// タブ名。None ならフォルダ名 (新規タブと同じ)。
    pub title: Option<String>,
}

/// コマンドライン引数の解析結果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cli {
    /// 最初のペインの cwd。None なら current_dir。
    pub cwd: Option<PathBuf>,
    /// 1 セッション 1 タブで開き直すセッション。空なら従来どおり新規 1 ペイン。
    pub resumes: Vec<ResumeSpec>,
}

/// `ccnest [cwd] [--resume <uuid>[=<タブ名>]]...` を解析する純粋関数
/// (`args` はプログラム名を除いたもの)。
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Cli> {
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
        } else if arg.starts_with("--") {
            return Err(anyhow!("unknown option: {arg}"));
        } else if cli.cwd.is_none() {
            cli.cwd = Some(PathBuf::from(arg));
        } else {
            return Err(anyhow!("unexpected argument: {arg}"));
        }
    }
    Ok(cli)
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
    })
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
                },
                ResumeSpec {
                    session_id: Uuid::parse_str(ID2).unwrap(),
                    title: None,
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
}

// Version History
// ver0.1 - 2026-10-19 - First version: positional cwd + repeated
//                       `--resume <uuid>[=<title>]` (reopen saved sessions,
//                       one tab each).
