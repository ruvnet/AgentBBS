//! Cross-repo collaboration adapters — GitHub (`gh`) + Jujutsu (`jj`).
//!
//! AgentBBS is a place where humans and agents coordinate work; that work
//! increasingly lives in Git repos. Rather than reimplement a GitHub client or
//! a VCS, we drive the `gh` and `jj` CLIs through the same mockable
//! [`CommandRunner`](crate::adapter::CommandRunner) seam used for ruflo/agentdb
//! (ADR-0008). Production uses `TokioCommandRunner`; tests use
//! `FakeCommandRunner` and never spawn a process or hit the network.
//!
//! These adapters are pure command builders — they do **not** hold or read any
//! token. `gh` authenticates from its own keychain/`GH_TOKEN` in the server
//! environment; the token never flows through AgentBBS. Capability gating
//! (ADR-0004) is enforced at the call site, exactly as for the other adapters.
//!
//! ADR-0036.

use agentbbs_core::Result;

use crate::adapter::CommandRunner;

/// How to merge a pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMethod {
    /// `--squash`
    Squash,
    /// `--merge`
    Merge,
    /// `--rebase`
    Rebase,
}

impl MergeMethod {
    fn flag(self) -> &'static str {
        match self {
            MergeMethod::Squash => "--squash",
            MergeMethod::Merge => "--merge",
            MergeMethod::Rebase => "--rebase",
        }
    }
}

/// Drives the `gh` CLI for cross-repo GitHub collaboration (issues, PRs,
/// reviews). Read methods return raw stdout (often `--json`) for the caller to
/// parse; write methods return `gh`'s confirmation output.
pub struct GitHubAdapter<R: CommandRunner> {
    runner: R,
}

impl<R: CommandRunner> GitHubAdapter<R> {
    /// Wrap a runner.
    pub fn new(runner: R) -> Self {
        GitHubAdapter { runner }
    }

    async fn gh(&self, args: &[&str]) -> Result<String> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.runner.run("gh", &owned).await
    }

    /// `gh issue list --repo <repo> --json ...` — open issues as JSON.
    pub async fn issue_list(&self, repo: &str) -> Result<String> {
        self.gh(&[
            "issue",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--json",
            "number,title,labels",
        ])
        .await
    }

    /// `gh issue create --repo <repo> --title <t> --body <b>`.
    pub async fn issue_create(&self, repo: &str, title: &str, body: &str) -> Result<String> {
        self.gh(&[
            "issue", "create", "--repo", repo, "--title", title, "--body", body,
        ])
        .await
    }

    /// `gh issue comment <number> --repo <repo> --body <b>`.
    pub async fn issue_comment(&self, repo: &str, number: u64, body: &str) -> Result<String> {
        let n = number.to_string();
        self.gh(&["issue", "comment", &n, "--repo", repo, "--body", body])
            .await
    }

    /// `gh pr list --repo <repo> --json ...` — open PRs as JSON.
    pub async fn pr_list(&self, repo: &str) -> Result<String> {
        self.gh(&[
            "pr",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--json",
            "number,title,headRefName,mergeable",
        ])
        .await
    }

    /// `gh pr create --repo <repo> --title <t> --body <b> --head <head> --base <base>`.
    pub async fn pr_create(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<String> {
        self.gh(&[
            "pr", "create", "--repo", repo, "--title", title, "--body", body, "--head", head,
            "--base", base,
        ])
        .await
    }

    /// `gh pr comment <number> --repo <repo> --body <b>` — a review note.
    pub async fn pr_comment(&self, repo: &str, number: u64, body: &str) -> Result<String> {
        let n = number.to_string();
        self.gh(&["pr", "comment", &n, "--repo", repo, "--body", body])
            .await
    }

    /// `gh pr merge <number> --repo <repo> <method>` — merge a reviewed PR.
    pub async fn pr_merge(&self, repo: &str, number: u64, method: MergeMethod) -> Result<String> {
        let n = number.to_string();
        self.gh(&["pr", "merge", &n, "--repo", repo, method.flag()])
            .await
    }
}

/// Drives the `jj` (Jujutsu) CLI for agentic VCS workflows — the development
/// side of cross-repo collaboration. Read-mostly plus safe authoring ops.
pub struct JujutsuAdapter<R: CommandRunner> {
    runner: R,
}

impl<R: CommandRunner> JujutsuAdapter<R> {
    /// Wrap a runner.
    pub fn new(runner: R) -> Self {
        JujutsuAdapter { runner }
    }

    async fn jj(&self, args: &[&str]) -> Result<String> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.runner.run("jj", &owned).await
    }

    /// `jj status` — the working-copy summary.
    pub async fn status(&self) -> Result<String> {
        self.jj(&["status"]).await
    }

    /// `jj diff` — changes in the working copy.
    pub async fn diff(&self) -> Result<String> {
        self.jj(&["diff"]).await
    }

    /// `jj log -n <limit>` — recent change history.
    pub async fn log(&self, limit: u32) -> Result<String> {
        let n = limit.to_string();
        self.jj(&["log", "-n", &n]).await
    }

    /// `jj new -m <message>` — start a new change.
    pub async fn new_change(&self, message: &str) -> Result<String> {
        self.jj(&["new", "-m", message]).await
    }

    /// `jj describe -m <message>` — set the current change's description.
    pub async fn describe(&self, message: &str) -> Result<String> {
        self.jj(&["describe", "-m", message]).await
    }

    /// `jj git push` — push changes to the Git remote.
    pub async fn git_push(&self) -> Result<String> {
        self.jj(&["git", "push"]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::FakeCommandRunner;

    #[tokio::test]
    async fn github_builds_expected_commands() {
        let fake = FakeCommandRunner::with_output("ok");
        let gh = GitHubAdapter::new(fake.clone());

        gh.issue_comment("ruvnet/AgentBBS", 6, "status update")
            .await
            .unwrap();
        gh.pr_merge("ruvnet/AgentBBS", 5, MergeMethod::Squash)
            .await
            .unwrap();
        gh.pr_create("o/r", "T", "B", "feat/x", "main")
            .await
            .unwrap();

        let calls = fake.calls();
        assert_eq!(
            calls[0],
            vec![
                "gh",
                "issue",
                "comment",
                "6",
                "--repo",
                "ruvnet/AgentBBS",
                "--body",
                "status update"
            ]
        );
        assert_eq!(
            calls[1],
            vec![
                "gh",
                "pr",
                "merge",
                "5",
                "--repo",
                "ruvnet/AgentBBS",
                "--squash"
            ]
        );
        assert_eq!(
            calls[2],
            vec![
                "gh", "pr", "create", "--repo", "o/r", "--title", "T", "--body", "B", "--head",
                "feat/x", "--base", "main"
            ]
        );
    }

    #[tokio::test]
    async fn github_list_returns_runner_output() {
        let fake = FakeCommandRunner::with_output(r#"[{"number":6}]"#);
        let gh = GitHubAdapter::new(fake);
        let out = gh.issue_list("o/r").await.unwrap();
        assert_eq!(out, r#"[{"number":6}]"#);
    }

    #[tokio::test]
    async fn jujutsu_builds_expected_commands() {
        let fake = FakeCommandRunner::with_output("");
        let jj = JujutsuAdapter::new(fake.clone());

        jj.new_change("feat: pods").await.unwrap();
        jj.log(5).await.unwrap();
        jj.git_push().await.unwrap();

        let calls = fake.calls();
        assert_eq!(calls[0], vec!["jj", "new", "-m", "feat: pods"]);
        assert_eq!(calls[1], vec!["jj", "log", "-n", "5"]);
        assert_eq!(calls[2], vec!["jj", "git", "push"]);
    }
}
