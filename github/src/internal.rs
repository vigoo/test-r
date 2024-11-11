use gh_workflow::{Expression, Job, Run, RunsOn, Step, Strategy, Use};
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
        self.runs_on = Some(RunsOn::from(runs_on.to_string()));
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

impl From<InstallAction> for Step<Use> {
    fn from(action: InstallAction) -> Self {
        let mut step = Step::uses("taiki-e", "install-action", 2);
        if !action.checksum {
            step = step.add_with(("checksum", "false"));
        }
        step = step.add_with(("tool", action.tools.join(",")));
        step
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

pub struct SetupMDBook {
    version: String,
}

impl SetupMDBook {
    pub fn version(mut self, version: impl ToString) -> Self {
        self.version = version.to_string();
        self
    }
}

impl Default for SetupMDBook {
    fn default() -> Self {
        Self {
            version: "latest".to_string(),
        }
    }
}

impl From<SetupMDBook> for Step<Use> {
    fn from(action: SetupMDBook) -> Self {
        Step::uses("peaceiris", "actions-mdbook", 2).with(("mdbook-version", action.version))
    }
}

#[derive(Debug, Default)]
pub struct GHPages {
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

impl GHPages {
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

impl From<GHPages> for Step<Use> {
    fn from(action: GHPages) -> Self {
        let mut step = Step::uses("peaceiris", "actions-gh-pages", 4);
        if let Some(allow_empty_commit) = action.allow_empty_commit {
            step = step.add_with(("allow_empty_commit", allow_empty_commit.to_string()));
        }
        if let Some(commit_message) = action.commit_message {
            step = step.add_with(("commit_message", commit_message));
        }
        if let Some(cname) = action.cname {
            step = step.add_with(("cname", cname));
        }
        if let Some(deploy_key) = action.deploy_key {
            step = step.add_with(("deploy_key", deploy_key));
        }
        if let Some(destination_dir) = action.destination_dir {
            step = step.add_with(("destination_dir", destination_dir));
        }
        if let Some(enable_jekyll) = action.enable_jekyll {
            step = step.add_with(("enable_jekyll", enable_jekyll.to_string()));
        }
        if let Some(exclude_assets) = action.exclude_assets {
            step = step.add_with(("exclude_assets", exclude_assets.join(",")));
        }
        if let Some(external_repository) = action.external_repository {
            step = step.add_with(("external_repository", external_repository));
        }
        if let Some(force_orphan) = action.force_orphan {
            step = step.add_with(("force_orphan", force_orphan.to_string()));
        }
        if let Some(full_commit_message) = action.full_commit_message {
            step = step.add_with(("full_commit_message", full_commit_message));
        }
        if let Some(github_token) = action.github_token {
            step = step.add_with(("github_token", github_token));
        }
        if let Some(keep_files) = action.keep_files {
            step = step.add_with(("keep_files", keep_files.to_string()));
        }
        if let Some(personal_token) = action.personal_token {
            step = step.add_with(("personal_token", personal_token));
        }
        if let Some(publish_branch) = action.publish_branch {
            step = step.add_with(("publish_branch", publish_branch));
        }
        if let Some(publish_dir) = action.publish_dir {
            step = step.add_with(("publish_dir", publish_dir));
        }
        if let Some(tag_name) = action.tag_name {
            step = step.add_with(("tag_name", tag_name));
        }
        if let Some(tag_message) = action.tag_message {
            step = step.add_with(("tag_message", tag_message));
        }
        if let Some(user_name) = action.user_name {
            step = step.add_with(("user_name", user_name));
        }
        if let Some(user_email) = action.user_email {
            step = step.add_with(("user_email", user_email));
        }

        if let Some(if_condition) = action.if_condition {
            step = step.if_condition(if_condition);
        }

        step
    }
}

pub trait StepExt {
    fn ghpages() -> GHPages;
    fn install_action() -> InstallAction;
    fn setup_mdbook() -> SetupMDBook;
}

pub trait StepRunExt {
    fn working_directory_(self, working_directory: impl ToString) -> Self;
}

impl StepExt for Step<Use> {
    fn ghpages() -> GHPages {
        GHPages::default()
    }

    fn install_action() -> InstallAction {
        InstallAction::default()
    }

    fn setup_mdbook() -> SetupMDBook {
        SetupMDBook::default()
    }
}

impl StepRunExt for Step<Run> {
    fn working_directory_(self, working_directory: impl ToString) -> Self {
        self.working_directory(working_directory.to_string())
    }
}
