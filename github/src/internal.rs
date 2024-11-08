use gh_workflow::{AddStep, Expression, Job, Run, Step, Strategy, Use};
use serde_json::{Map, Value};

#[derive(Debug, Default)]
pub struct MatrixDimension {
    key: String,
    values: Vec<String>,
}

impl MatrixDimension {
    pub fn new(key: impl ToString) -> Self {
        Self {
            key: key.to_string(),
            values: Vec::new(),
        }
    }

    pub fn value(mut self, value: impl ToString) -> Self {
        self.values.push(value.to_string());
        self
    }
}

#[derive(Debug, Default)]
pub struct Matrix {
    dimensions: Vec<MatrixDimension>,
}

impl Matrix {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn add_dimension(mut self, dimension: MatrixDimension) -> Self {
        self.dimensions.push(dimension);
        self
    }

    pub fn into_value(self) -> Value {
        Value::Object(Map::from_iter(self.dimensions.into_iter().map(|dim| {
            (
                dim.key,
                Value::Array(dim.values.into_iter().map(Value::String).collect()),
            )
        })))
    }
}

pub fn matrix(builder: Matrix) -> Strategy {
    Strategy {
        matrix: Some(builder.into_value()),
        ..Strategy::default()
    }
}

pub trait JobExt {
    fn runs_on_(self, runs_on: impl ToString) -> Self;
}

impl JobExt for Job {
    fn runs_on_(mut self, runs_on: impl ToString) -> Self {
        self.runs_on = Some(Value::String(runs_on.to_string()));
        self
    }
}

#[derive(Debug)]
pub struct InstallAction {
    tools: Vec<String>,
    checksum: bool,
}

impl InstallAction {
    pub fn add_tool(mut self, tool: impl ToString) -> Self {
        self.tools.push(tool.to_string());
        self
    }

    pub fn checksum(mut self, checksum: bool) -> Self {
        self.checksum = checksum;
        self
    }
}

impl AddStep for InstallAction {
    fn apply(self, job: Job) -> Job {
        let mut step = Step::uses("taiki-e", "install-action", 2);
        if !self.checksum {
            step = step.with(("checksum", "false"));
        }
        step = step.with(("tool", self.tools.join(",")));

        job.add_step(step)
    }
}

impl Default for InstallAction {
    fn default() -> Self {
        Self {
            tools: Vec::new(),
            checksum: true,
        }
    }
}

pub struct SetupMdbook {
    version: String,
}

impl SetupMdbook {
    pub fn version(mut self, version: impl ToString) -> Self {
        self.version = version.to_string();
        self
    }
}

impl Default for SetupMdbook {
    fn default() -> Self {
        Self {
            version: "latest".to_string(),
        }
    }
}

impl AddStep for SetupMdbook {
    fn apply(self, job: Job) -> Job {
        job.add_step(
            Step::uses("peaceiris", "actions-mdbook", 2).with(("mdbook-version", self.version)),
        )
    }
}

#[derive(Debug, Default)]
pub struct GhPages {
    allow_empty_commit: Option<bool>,
    commit_message: Option<String>,
    cname: Option<String>,
    deploy_key: Option<String>,
    destination_dir: Option<String>,
    enable_jekyll: Option<bool>,
    exclude_assets: Option<Vec<String>>,
    external_repository: Option<String>,
    force_orphan: Option<bool>,
    full_commit_message: Option<String>,
    github_token: Option<String>,
    keep_files: Option<bool>,
    personal_token: Option<String>,
    publish_branch: Option<String>,
    publish_dir: Option<String>,
    tag_name: Option<String>,
    tag_message: Option<String>,
    user_name: Option<String>,
    user_email: Option<String>,

    if_condition: Option<Expression>,
}

impl GhPages {
    pub fn allow_empty_commit(mut self, allow_empty_commit: bool) -> Self {
        self.allow_empty_commit = Some(allow_empty_commit);
        self
    }

    pub fn commit_message(mut self, commit_message: impl ToString) -> Self {
        self.commit_message = Some(commit_message.to_string());
        self
    }

    pub fn cname(mut self, cname: impl ToString) -> Self {
        self.cname = Some(cname.to_string());
        self
    }

    pub fn deploy_key(mut self, deploy_key: impl ToString) -> Self {
        self.deploy_key = Some(deploy_key.to_string());
        self
    }

    pub fn destination_dir(mut self, destination_dir: impl ToString) -> Self {
        self.destination_dir = Some(destination_dir.to_string());
        self
    }

    pub fn enable_jekyll(mut self, enable_jekyll: bool) -> Self {
        self.enable_jekyll = Some(enable_jekyll);
        self
    }

    pub fn exclude_asset(mut self, asset: impl ToString) -> Self {
        match &mut self.exclude_assets {
            None => self.exclude_assets = Some(vec![asset.to_string()]),
            Some(exclude_assets) => {
                exclude_assets.push(asset.to_string());
            }
        };
        self
    }

    pub fn external_repository(mut self, external_repository: impl ToString) -> Self {
        self.external_repository = Some(external_repository.to_string());
        self
    }

    pub fn force_orphan(mut self, force_orphan: bool) -> Self {
        self.force_orphan = Some(force_orphan);
        self
    }

    pub fn full_commit_message(mut self, full_commit_message: impl ToString) -> Self {
        self.full_commit_message = Some(full_commit_message.to_string());
        self
    }

    pub fn github_token(mut self, github_token: impl ToString) -> Self {
        self.github_token = Some(github_token.to_string());
        self
    }

    pub fn keep_files(mut self, keep_files: bool) -> Self {
        self.keep_files = Some(keep_files);
        self
    }

    pub fn personal_token(mut self, personal_token: impl ToString) -> Self {
        self.personal_token = Some(personal_token.to_string());
        self
    }

    pub fn publish_branch(mut self, publish_branch: impl ToString) -> Self {
        self.publish_branch = Some(publish_branch.to_string());
        self
    }

    pub fn publish_dir(mut self, publish_dir: impl ToString) -> Self {
        self.publish_dir = Some(publish_dir.to_string());
        self
    }

    pub fn tag_name(mut self, tag_name: impl ToString) -> Self {
        self.tag_name = Some(tag_name.to_string());
        self
    }

    pub fn tag_message(mut self, tag_message: impl ToString) -> Self {
        self.tag_message = Some(tag_message.to_string());
        self
    }

    pub fn user_name(mut self, user_name: impl ToString) -> Self {
        self.user_name = Some(user_name.to_string());
        self
    }

    pub fn user_email(mut self, user_email: impl ToString) -> Self {
        self.user_email = Some(user_email.to_string());
        self
    }

    pub fn if_condition(mut self, if_condition: Expression) -> Self {
        self.if_condition = Some(if_condition);
        self
    }
}

impl AddStep for GhPages {
    fn apply(self, job: Job) -> Job {
        let mut step = Step::uses("peaceiris", "actions-gh-pages", 4);
        if let Some(allow_empty_commit) = self.allow_empty_commit {
            step = step.with(("allow_empty_commit", allow_empty_commit.to_string()));
        }
        if let Some(commit_message) = self.commit_message {
            step = step.with(("commit_message", commit_message));
        }
        if let Some(cname) = self.cname {
            step = step.with(("cname", cname));
        }
        if let Some(deploy_key) = self.deploy_key {
            step = step.with(("deploy_key", deploy_key));
        }
        if let Some(destination_dir) = self.destination_dir {
            step = step.with(("destination_dir", destination_dir));
        }
        if let Some(enable_jekyll) = self.enable_jekyll {
            step = step.with(("enable_jekyll", enable_jekyll.to_string()));
        }
        if let Some(exclude_assets) = self.exclude_assets {
            step = step.with(("exclude_assets", exclude_assets.join(",")));
        }
        if let Some(external_repository) = self.external_repository {
            step = step.with(("external_repository", external_repository));
        }
        if let Some(force_orphan) = self.force_orphan {
            step = step.with(("force_orphan", force_orphan.to_string()));
        }
        if let Some(full_commit_message) = self.full_commit_message {
            step = step.with(("full_commit_message", full_commit_message));
        }
        if let Some(github_token) = self.github_token {
            step = step.with(("github_token", github_token));
        }
        if let Some(keep_files) = self.keep_files {
            step = step.with(("keep_files", keep_files.to_string()));
        }
        if let Some(personal_token) = self.personal_token {
            step = step.with(("personal_token", personal_token));
        }
        if let Some(publish_branch) = self.publish_branch {
            step = step.with(("publish_branch", publish_branch));
        }
        if let Some(publish_dir) = self.publish_dir {
            step = step.with(("publish_dir", publish_dir));
        }
        if let Some(tag_name) = self.tag_name {
            step = step.with(("tag_name", tag_name));
        }
        if let Some(tag_message) = self.tag_message {
            step = step.with(("tag_message", tag_message));
        }
        if let Some(user_name) = self.user_name {
            step = step.with(("user_name", user_name));
        }
        if let Some(user_email) = self.user_email {
            step = step.with(("user_email", user_email));
        }

        if let Some(if_condition) = self.if_condition {
            step = step.if_condition(if_condition);
        }

        job.add_step(step)
    }
}

pub trait StepExt {
    fn ghpages() -> GhPages;
    fn install_action() -> InstallAction;
    fn setup_mdbook() -> SetupMdbook;
}

pub trait StepRunExt {
    fn working_directory_(self, working_directory: impl ToString) -> Self;
}

impl StepExt for Step<Use> {
    fn ghpages() -> GhPages {
        GhPages::default()
    }

    fn install_action() -> InstallAction {
        InstallAction::default()
    }

    fn setup_mdbook() -> SetupMdbook {
        SetupMdbook::default()
    }
}

impl StepRunExt for Step<Run> {
    fn working_directory_(self, working_directory: impl ToString) -> Self {
        self.working_directory(working_directory.to_string())
    }
}
