//! Human browsing groups existing catalog identities without re-resolving source policy.
use super::*;
impl InstructionInspector {
    fn family(resource: &Resource) -> (String, String) {
        let identity = if resource.row.kind == "skill" {
            resource
                .skill_name
                .clone()
                .unwrap_or_else(|| resource.row.key.clone())
        } else if resource.managed.is_some()
            || resource.alias.is_some()
            || resource.row.kind == "AGENTS.md"
        {
            resource.row.id.clone()
        } else {
            resource.row.key.clone()
        };
        (resource.row.kind.clone(), identity)
    }
    pub(super) fn alternatives(&self, resource: &Resource) -> Vec<&Resource> {
        let family = Self::family(resource);
        self.resources
            .values()
            .filter(|other| Self::family(other) == family)
            .collect()
    }
    pub(super) fn rows(&self, filter: &InstructionFilter, offset: usize) -> InstructionRowsPage {
        let search = filter.search.to_lowercase();
        let matches = |resource: &&Resource| {
            let row = &resource.row;
            (!filter.main_catalog
                || (row.origin != InstructionOrigin::Legacy && row.kind != "store-settings"))
                && filter.kind.as_ref().is_none_or(|kind| kind == &row.kind)
                && filter
                    .scope
                    .as_ref()
                    .is_none_or(|scope| scope == &row.scope)
                && filter
                    .repository
                    .as_ref()
                    .is_none_or(|repository| repository == &row.repository)
                && filter.origin.is_none_or(|origin| origin == row.origin)
                && filter
                    .effective
                    .is_none_or(|effective| effective == row.effective)
                && filter.valid.is_none_or(|valid| valid == row.valid)
                && filter
                    .redefinitions
                    .is_none_or(|value| value == row.redefines_global)
                && (search.is_empty()
                    || [&row.id, &row.name, &row.kind, &row.scope, &row.description]
                        .iter()
                        .any(|value| value.to_lowercase().contains(&search)))
        };
        let matching = self.resources.values().filter(matches).collect::<Vec<_>>();
        let mut representatives = if filter.grouped {
            let mut groups = BTreeMap::<(String, String), Vec<&Resource>>::new();
            for resource in matching {
                groups
                    .entry(Self::family(resource))
                    .or_default()
                    .push(resource);
            }
            groups
                .into_values()
                .map(|mut group| {
                    // Preserve the owning runtime's effective flags, including paired
                    // additive contributions and invalid candidates. Other definitions
                    // remain explicit alternatives, not discarded duplicate rows.
                    group.sort_by_key(|resource| {
                        (
                            !resource.row.effective,
                            resource.row.scope != "project",
                            resource.row.origin != InstructionOrigin::Managed,
                            resource.row.key.clone(),
                        )
                    });
                    group[0]
                })
                .collect::<Vec<_>>()
        } else {
            matching
        };
        representatives.sort_by_key(|resource| {
            (
                resource.row.name.to_lowercase(),
                resource.row.id.clone(),
                resource.row.key.clone(),
            )
        });
        let total = representatives.len();
        let offset = offset.min(total);
        let rows = representatives
            .into_iter()
            .skip(offset)
            .take(ROW_PAGE_SIZE)
            .map(|resource| {
                let mut row = resource.row.clone();
                row.variants = self
                    .alternatives(resource)
                    .into_iter()
                    .map(|variant| InstructionSourceVariant {
                        key: variant.row.key.clone(),
                        name: variant.row.name.clone(),
                        scope: variant.row.scope.clone(),
                        repository: variant.row.repository.clone(),
                        origin: variant.row.origin,
                        path: variant.path.display().to_string(),
                        effective: variant.row.effective,
                        valid: variant.row.valid,
                    })
                    .collect();
                row
            })
            .collect::<Vec<_>>();
        let end = offset + rows.len();
        InstructionRowsPage {
            offset,
            total,
            next: (end < total).then_some(end),
            rows,
        }
    }
}
