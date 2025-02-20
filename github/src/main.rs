use gh_workflow::{
    Cargo, Event, Expression, Job, Level, Permissions, PullRequest, Push, Step, Workflow,
    toolchain::Toolchain,
};
use internal::*;

#[allow(dead_code)]
mod internal;

fn main() {
    let toolchain = Toolchain::default();

    Workflow::new("CI")
        .on(Event::default()
            .push(Push::default().add_branch("master"))
            .pull_request(PullRequest::default()))
        .permissions(Permissions::default().contents(Level::Write))
        .add_job(
            "build-and-test",
            Job::new("Build and test")
                .strategy(matrix(
                    Matrix::empty().add_dimension(
                        MatrixDimension::new("os")
                            .value("ubuntu-latest")
                            .value("windows-latest")
                            .value("macos-latest"),
                    ),
                ))
                .runs_on_("${{ matrix.os }}")
                .add_step(Step::checkout())
                .add_step(toolchain.clone())
                .add_step(Cargo::new("test").args("-p test-r --all-features"))
                .add_step(Cargo::new("test").args("-p test-r-example --no-run"))
                .add_step(Cargo::new("test").args("-p test-r-example-tokio --no-run")),
        )
        .add_job(
            "checks",
            Job::new("Checks")
                .runs_on_("ubuntu-latest")
                .add_step(Step::checkout())
                .add_step(toolchain)
                .add_step(InstallAction::default().add_tool("cargo-deny"))
                .add_step(Cargo::new("clippy").args("--no-deps --all-targets -- -Dwarnings"))
                .add_step(Cargo::new("fmt").args("--all -- --check"))
                .add_step(Cargo::new("deny").args("check")),
        )
        .add_job(
            "deploy-book",
            Job::new("Deploy book")
                .runs_on_("ubuntu-latest")
                .add_step(Step::checkout())
                .add_step(SetupMDBook::default())
                .add_step(Step::run("mdbook build").working_directory_("book"))
                .add_step(
                    GHPages::default()
                        .if_condition(Expression::new("${{ github.ref == 'refs/heads/master' }}"))
                        .github_token("${{ secrets.GITHUB_TOKEN }}")
                        .publish_dir("./book/book")
                        .cname("test-r.vigoo.dev"),
                ),
        )
        .generate()
        .unwrap();
}
