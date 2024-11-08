use gh_workflow::{Event, Expression, Job, PermissionLevel, Permissions, Step, Workflow};
use internal::*;

#[allow(dead_code)]
mod internal;

fn main() {
    Workflow::new("CI")
        .on(Event::push().branch("master"))
        .on(Event::pull_request())
        .permissions(Permissions::default().contents(PermissionLevel::Write))
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
                .add_step(Step::setup_rust())
                .add_step(Step::cargo("test", vec!["-p", "test-r", "--all-features"]))
                .add_step(Step::cargo(
                    "test",
                    vec!["-p", "test-r-example", "--no-run"],
                ))
                .add_step(Step::cargo(
                    "test",
                    vec!["-p", "test-r-example-tokio", "--no-run"],
                )),
        )
        .add_job(
            "checks",
            Job::new("Checks")
                .runs_on_("ubuntu-latest")
                .add_step(Step::checkout())
                .add_step(Step::setup_rust())
                .add_step(Step::install_action().add_tool("cargo-deny"))
                .add_step(Step::cargo(
                    "clippy",
                    vec!["--no-deps", "--all-targets", "--", "-Dwarnings"],
                ))
                .add_step(Step::cargo("fmt", vec!["--all", "--", "--check"]))
                .add_step(Step::cargo("deny", vec!["check"])),
        )
        .add_job(
            "deploy-book",
            Job::new("Deploy book")
                .runs_on_("ubuntu-latest")
                .add_step(Step::checkout())
                .add_step(Step::setup_mdbook())
                .add_step(Step::run("mdbook build").working_directory_("book"))
                .add_step(
                    Step::ghpages()
                        .if_condition(Expression::new("${{ github.ref == 'refs/heads/master' }}"))
                        .github_token("${{ secrets.GITHUB_TOKEN }}")
                        .publish_dir("./book/book")
                        .cname("test-r.vigoo.dev"),
                ),
        )
        .generate()
        .unwrap();
}
