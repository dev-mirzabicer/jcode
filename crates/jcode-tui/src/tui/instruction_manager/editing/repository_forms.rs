#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum RepositoryMode {
    Initialize,
    Recreate,
    Submodule,
    Clone,
    Attach,
    Standalone,
    Repair,
    Remote,
    Checkout,
    CreateBranch,
    Fetch,
    Pull,
    Push,
    FetchBranch,
}
impl RepositoryMode {
    fn all() -> Vec<Self> {
        vec![
            Self::Initialize,
            Self::Recreate,
            Self::Submodule,
            Self::Clone,
            Self::Attach,
            Self::Standalone,
            Self::Repair,
            Self::Remote,
            Self::Checkout,
            Self::CreateBranch,
            Self::Fetch,
            Self::Pull,
            Self::Push,
            Self::FetchBranch,
        ]
    }
    fn label(self) -> &'static str {
        match self {
            Self::Initialize => "Initialize global store",
            Self::Recreate => "Recreate from shipped seed",
            Self::Submodule => "Set up project submodule",
            Self::Clone => "Clone external repository",
            Self::Attach => "Attach existing checkout",
            Self::Standalone => "Set up standalone repository",
            Self::Repair => "Repair missing checkout",
            Self::Remote => "Configure remote",
            Self::Checkout => "Check out local branch",
            Self::CreateBranch => "Create working branch",
            Self::Fetch => "Fetch remote",
            Self::Pull => "Pull fast-forward only",
            Self::Push => "Push reviewed commits",
            Self::FetchBranch => "Fetch and select remote branch",
        }
    }
    fn available(self, choices: &InstructionRepositoryChoices) -> bool {
        match self {
            Self::Initialize => choices.can_initialize,
            Self::Recreate => choices.can_recreate,
            Self::Submodule => {
                choices.scope == InstructionEditScope::Project && choices.project_is_git
            }
            Self::Standalone => {
                choices.scope == InstructionEditScope::Project && !choices.project_is_git
            }
            Self::Clone | Self::Attach => choices.scope == InstructionEditScope::Project,
            Self::Repair => {
                choices.scope == InstructionEditScope::Project && choices.root.is_some()
            }
            _ => choices.git_available,
        }
    }
}
impl EditForm {
    pub fn repository(choices: InstructionRepositoryChoices) -> Self {
        let mode = if choices.can_initialize {
            RepositoryMode::Initialize
        } else if choices.git_available {
            RepositoryMode::Remote
        } else if choices.scope == InstructionEditScope::Global {
            RepositoryMode::Recreate
        } else if choices.project_is_git {
            RepositoryMode::Submodule
        } else {
            RepositoryMode::Standalone
        };
        let mut form = Self {
            anchor: None,
            title: format!("{:?} repository controls", choices.scope),
            fields: Vec::new(),
            selected: 0,
            picker: None,
            error: choices.warnings.join(" · "),
            buttons: vec![FormButton::Submit],
            offset: 0,
            purpose: Purpose::Repository(Box::new(choices), mode),
            choices: InstructionEditChoices::default(),
            last_field: 0,
        };
        form.load_repository_mode(mode);
        form
    }
    fn load_repository_mode(&mut self, mode: RepositoryMode) {
        let Purpose::Repository(choices, selected_mode) = &mut self.purpose else {
            return;
        };
        *selected_mode = mode;
        let branch = choices
            .current_branch
            .clone()
            .or(choices.configured_branch.clone())
            .unwrap_or_else(|| "main".into());
        let remote = choices
            .remotes
            .first()
            .map(|remote| remote.name.clone())
            .unwrap_or_else(|| "origin".into());
        let remote_names = choices
            .remotes
            .iter()
            .map(|remote| remote.name.clone())
            .collect::<Vec<_>>();
        self.fields = vec![Field::choice(
            FieldKey::RepositoryMode,
            "Operation",
            mode.label().into(),
            RepositoryMode::all()
                .into_iter()
                .filter(|mode| mode.available(choices))
                .map(|mode| mode.label().into())
                .collect(),
        )];
        match mode {
            RepositoryMode::Submodule => {
                self.fields.push(Field::text(
                    FieldKey::RepositoryPath,
                    "Submodule path (project-relative)",
                    ".jcode/instructions".into(),
                ));
                self.fields.push(Field::text(
                    FieldKey::RepositoryUrl,
                    "Repository URL (no credentials)",
                    String::new(),
                ));
                self.fields
                    .push(Field::text(FieldKey::Branch, "Working branch", branch));
            }
            RepositoryMode::Clone => {
                self.fields.push(Field::text(
                    FieldKey::RepositoryUrl,
                    "Repository URL (no credentials)",
                    String::new(),
                ));
                self.fields
                    .push(Field::text(FieldKey::Branch, "Working branch", branch));
            }
            RepositoryMode::Attach => {
                self.fields.push(Field::text(
                    FieldKey::RepositoryPath,
                    "Existing server checkout (absolute)",
                    String::new(),
                ));
                self.fields.push(Field::text(
                    FieldKey::Branch,
                    "Expected branch (empty: current)",
                    String::new(),
                ));
            }
            RepositoryMode::Standalone => self.fields.push(Field::text(
                FieldKey::RepositoryPath,
                "Repository path (project-relative)",
                ".jcode/instructions".into(),
            )),
            RepositoryMode::Remote => {
                self.fields.push(Field::choice(
                    FieldKey::Remote,
                    "Remote name",
                    remote,
                    remote_names,
                ));
                self.fields.push(Field::text(
                    FieldKey::RepositoryUrl,
                    "URL (no credentials)",
                    choices
                        .remotes
                        .first()
                        .map(|remote| remote.url.clone())
                        .unwrap_or_default(),
                ));
            }
            RepositoryMode::Checkout => self.fields.push(Field::choice(
                FieldKey::Branch,
                "Existing local branch",
                branch,
                choices.branches.clone(),
            )),
            RepositoryMode::CreateBranch => {
                self.fields.push(Field::text(
                    FieldKey::Branch,
                    "New local branch",
                    String::new(),
                ));
                self.fields.push(Field::choice(
                    FieldKey::Start,
                    "Start commit or reference",
                    "HEAD".into(),
                    std::iter::once("HEAD".into())
                        .chain(choices.branches.iter().cloned())
                        .chain(choices.remote_branches.iter().cloned())
                        .collect(),
                ));
            }
            RepositoryMode::Fetch => self.fields.push(Field::choice(
                FieldKey::Remote,
                "Remote",
                remote,
                remote_names,
            )),
            RepositoryMode::Pull | RepositoryMode::Push => {
                self.fields.push(Field::choice(
                    FieldKey::Remote,
                    "Remote",
                    remote,
                    remote_names,
                ));
                self.fields.push(Field::choice(
                    FieldKey::Branch,
                    "Branch",
                    branch,
                    choices.branches.clone(),
                ));
            }
            RepositoryMode::FetchBranch => {
                self.fields.push(Field::choice(
                    FieldKey::Remote,
                    "Remote",
                    remote.clone(),
                    remote_names,
                ));
                self.fields.push(Field::choice(
                    FieldKey::Branch,
                    "Remote branch",
                    branch,
                    choices
                        .remote_branches
                        .iter()
                        .filter_map(|branch| {
                            branch
                                .strip_prefix(&format!("{remote}/"))
                                .map(str::to_string)
                        })
                        .collect(),
                ));
                self.fields.push(Field::text(
                    FieldKey::LocalBranch,
                    "New local working branch",
                    String::new(),
                ));
            }
            _ => {}
        }
        self.selected = 0;
        self.last_field = 0;
        self.offset = 0;
    }
    fn repository_action(&self, mode: RepositoryMode) -> InstructionRepositoryAction {
        let value = |field| self.value(field);
        match mode {
            RepositoryMode::Initialize => InstructionRepositoryAction::InitializeGlobal,
            RepositoryMode::Recreate => InstructionRepositoryAction::RecreateGlobal,
            RepositoryMode::Submodule => InstructionRepositoryAction::Submodule {
                path: value(FieldKey::RepositoryPath),
                url: value(FieldKey::RepositoryUrl),
                branch: value(FieldKey::Branch),
            },
            RepositoryMode::Clone => InstructionRepositoryAction::CloneExternal {
                url: value(FieldKey::RepositoryUrl),
                branch: value(FieldKey::Branch),
            },
            RepositoryMode::Attach => InstructionRepositoryAction::AttachExternal {
                path: value(FieldKey::RepositoryPath),
                branch: optional(value(FieldKey::Branch)),
            },
            RepositoryMode::Standalone => InstructionRepositoryAction::Standalone {
                path: value(FieldKey::RepositoryPath),
            },
            RepositoryMode::Repair => InstructionRepositoryAction::RepairCheckout,
            RepositoryMode::Remote => InstructionRepositoryAction::ConfigureRemote {
                name: value(FieldKey::Remote),
                url: value(FieldKey::RepositoryUrl),
            },
            RepositoryMode::Checkout => InstructionRepositoryAction::Checkout {
                branch: value(FieldKey::Branch),
                create: false,
                start: None,
            },
            RepositoryMode::CreateBranch => InstructionRepositoryAction::Checkout {
                branch: value(FieldKey::Branch),
                create: true,
                start: optional(value(FieldKey::Start)),
            },
            RepositoryMode::Fetch => InstructionRepositoryAction::Fetch {
                remote: value(FieldKey::Remote),
            },
            RepositoryMode::Pull => InstructionRepositoryAction::Pull {
                remote: value(FieldKey::Remote),
                branch: value(FieldKey::Branch),
            },
            RepositoryMode::Push => InstructionRepositoryAction::Push {
                remote: value(FieldKey::Remote),
                branch: value(FieldKey::Branch),
            },
            RepositoryMode::FetchBranch => InstructionRepositoryAction::FetchCheckout {
                remote: value(FieldKey::Remote),
                branch: value(FieldKey::Branch),
                local_branch: value(FieldKey::LocalBranch),
            },
        }
    }
}
