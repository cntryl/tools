use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use proc_macro2::{Span as ProcSpan, TokenStream, TokenTree};
use quote::ToTokens;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprCall, ExprLit, ExprMethodCall, ExprPath, ExprTry, Item, ItemFn, Lit, Meta,
    Pat, Path as SynPath, Token,
};

use super::config::TestPolicyConfig;
use super::model::{
    AnalysisGap, CallFact, HygieneFact, HygieneKind, Marker, MarkerKind, OracleFact, OracleKind,
    Position, Section, SourceLocation, SourceSpan, TestCase, TestIdentity,
};

const GLOB_IMPORT_SENTINEL: &str = "\0cntryl-glob-import";

pub fn analyze_source(
    relative_path: &str,
    source: &str,
    policy: &TestPolicyConfig,
) -> Result<Vec<TestCase>> {
    let file = syn::parse_file(source)?;
    let lines: Vec<&str> = source.lines().collect();
    let mut tests = Vec::new();
    collect_items(
        relative_path,
        &file.items,
        &[],
        &BTreeMap::new(),
        &lines,
        policy,
        &mut tests,
    );
    Ok(tests)
}

fn collect_items(
    relative_path: &str,
    items: &[Item],
    module_path: &[String],
    parent_aliases: &BTreeMap<String, String>,
    lines: &[&str],
    policy: &TestPolicyConfig,
    tests: &mut Vec<TestCase>,
) {
    let mut aliases = BTreeMap::new();
    collect_item_use_aliases(items, &mut aliases);
    resolve_parent_aliases(&mut aliases, parent_aliases);
    for item in items {
        match item {
            Item::Fn(function) if is_test(function, policy, &aliases) => tests.push(analyze_test(
                relative_path,
                function,
                module_path,
                &aliases,
                lines,
                policy,
            )),
            Item::Mod(module) => {
                if let Some((_, nested)) = &module.content {
                    let mut nested_path = module_path.to_vec();
                    nested_path.push(module.ident.to_string());
                    collect_items(
                        relative_path,
                        nested,
                        &nested_path,
                        &aliases,
                        lines,
                        policy,
                        tests,
                    );
                }
            }
            _ => {}
        }
    }
}

fn resolve_parent_aliases(
    aliases: &mut BTreeMap<String, String>,
    parent_aliases: &BTreeMap<String, String>,
) {
    for target in aliases.values_mut() {
        let Some(relative) = target.strip_prefix("super::") else {
            continue;
        };
        let first = relative.split("::").next().unwrap_or(relative);
        if parent_aliases.contains_key(first) {
            *target = resolve_alias(relative, parent_aliases);
        }
    }
}

fn is_test(
    function: &ItemFn,
    policy: &TestPolicyConfig,
    aliases: &BTreeMap<String, String>,
) -> bool {
    function
        .attrs
        .iter()
        .map(|attribute| path_name(attribute.path()))
        .map(|name| resolve_alias(&name, aliases))
        .any(|name| policy.matches_test_attribute(&name))
}

fn resolve_alias(name: &str, aliases: &BTreeMap<String, String>) -> String {
    let mut resolved_name = name.to_string();
    let mut seen = BTreeSet::new();
    while seen.insert(resolved_name.clone()) {
        let mut segments = resolved_name.split("::");
        let Some(first) = segments.next() else {
            break;
        };
        let Some(resolved) = aliases.get(first) else {
            break;
        };
        let remainder = segments.collect::<Vec<_>>().join("::");
        resolved_name = if remainder.is_empty() {
            resolved.clone()
        } else {
            format!("{resolved}::{remainder}")
        };
    }
    resolved_name
}

fn collect_item_use_aliases(items: &[Item], aliases: &mut BTreeMap<String, String>) {
    for item in items {
        match item {
            Item::Use(import) => collect_use_tree(&import.tree, &[], aliases),
            Item::Type(alias) => {
                collect_type_alias(alias, aliases);
            }
            _ => {}
        }
    }
}

fn collect_type_alias(alias: &syn::ItemType, aliases: &mut BTreeMap<String, String>) -> bool {
    let syn::Type::Path(path) = &*alias.ty else {
        return false;
    };
    if path.qself.is_some() {
        return false;
    }
    aliases.insert(alias.ident.to_string(), path_name(&path.path));
    true
}

fn collect_use_tree(
    tree: &syn::UseTree,
    prefix: &[String],
    aliases: &mut BTreeMap<String, String>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            let mut nested_prefix = prefix.to_vec();
            nested_prefix.push(path.ident.to_string());
            collect_use_tree(&path.tree, &nested_prefix, aliases);
        }
        syn::UseTree::Name(name) => {
            if name.ident == "self" {
                if let Some(alias) = prefix.last() {
                    aliases.insert(alias.clone(), prefix.join("::"));
                }
            } else {
                let mut qualified = prefix.to_vec();
                qualified.push(name.ident.to_string());
                aliases.insert(name.ident.to_string(), qualified.join("::"));
            }
        }
        syn::UseTree::Rename(rename) => {
            let mut qualified = prefix.to_vec();
            qualified.push(rename.ident.to_string());
            aliases.insert(rename.rename.to_string(), qualified.join("::"));
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                collect_use_tree(item, prefix, aliases);
            }
        }
        syn::UseTree::Glob(_) => {
            aliases.insert(GLOB_IMPORT_SENTINEL.into(), prefix.join("::"));
        }
    }
}

fn item_binding_name(item: &Item) -> Option<String> {
    let identifier = match item {
        Item::Const(item) => &item.ident,
        Item::Enum(item) => &item.ident,
        Item::ExternCrate(item) => &item.ident,
        Item::Fn(item) => &item.sig.ident,
        Item::Macro(item) => return item.ident.as_ref().map(ToString::to_string),
        Item::Mod(item) => &item.ident,
        Item::Static(item) => &item.ident,
        Item::Struct(item) => &item.ident,
        Item::Trait(item) => &item.ident,
        Item::TraitAlias(item) => &item.ident,
        Item::Type(item) => &item.ident,
        Item::Union(item) => &item.ident,
        _ => return None,
    };
    Some(identifier.to_string())
}

fn has_observable_test_return(output: &syn::ReturnType) -> bool {
    let syn::ReturnType::Type(_, output) = output else {
        return false;
    };
    !matches!(&**output, syn::Type::Tuple(tuple) if tuple.elems.is_empty())
}

fn is_unconditional_success(expression: &Expr) -> bool {
    match expression {
        Expr::Call(call) => callable_name(&call.func)
            .as_deref()
            .is_some_and(|name| name.rsplit("::").next() == Some("Ok")),
        Expr::Path(path) => path_name(&path.path).ends_with("ExitCode::SUCCESS"),
        Expr::Tuple(tuple) => tuple.elems.is_empty(),
        Expr::Group(group) => is_unconditional_success(&group.expr),
        Expr::Paren(parenthesized) => is_unconditional_success(&parenthesized.expr),
        _ => false,
    }
}

fn analyze_test(
    relative_path: &str,
    function: &ItemFn,
    module_path: &[String],
    inherited_aliases: &BTreeMap<String, String>,
    lines: &[&str],
    policy: &TestPolicyConfig,
) -> TestCase {
    let name = function.sig.ident.to_string();
    let qualified_name = if module_path.is_empty() {
        name.clone()
    } else {
        format!("{}::{name}", module_path.join("::"))
    };
    let function_span = SourceSpan {
        start: source_span(function.sig.fn_token.span).start,
        end: source_span(function.block.brace_token.span.close()).end,
    };
    let name_span = source_span(function.sig.ident.span());
    let markers = find_markers(lines, function_span);
    let (ignored, ignore_span) = ignored_attribute(&function.attrs);
    let (should_panic, should_panic_expected) = should_panic_attribute(&function.attrs);
    let virtual_time = uses_paused_tokio_time(&function.attrs, inherited_aliases);
    let identity = TestIdentity {
        name: name.clone(),
        qualified_name: qualified_name.clone(),
    };
    let observable_test_return = has_observable_test_return(&function.sig.output);
    let has_explicit_sections = !markers.is_empty();
    let mut parameter_bindings = BTreeSet::new();
    for input in &function.sig.inputs {
        if let syn::FnArg::Typed(argument) = input {
            identifiers_from_pattern(&argument.pat, &mut parameter_bindings);
        }
    }
    let mut use_aliases = inherited_aliases.clone();
    for statement in &function.block.stmts {
        if let syn::Stmt::Item(Item::Use(import)) = statement {
            collect_use_tree(&import.tree, &[], &mut use_aliases);
        }
    }
    let mut visitor = FactVisitor {
        relative_path,
        identity,
        markers: &markers,
        policy,
        has_explicit_sections,
        inside_oracle: false,
        control_flow_depth: 0,
        act_outputs: BTreeSet::new(),
        act_output_spans: BTreeMap::new(),
        act_effects: BTreeSet::new(),
        act_receivers: BTreeSet::new(),
        act_shared_references: BTreeSet::new(),
        act_root_calls: BTreeSet::new(),
        uncontrolled_clock_outputs: BTreeSet::new(),
        local_origins: BTreeMap::new(),
        local_callable_origins: BTreeMap::new(),
        bound_names: parameter_bindings,
        current_binding_identifiers: BTreeSet::new(),
        mutable_reference_aliases: BTreeMap::new(),
        observable_test_return,
        virtual_time,
        use_aliases,
        calls: Vec::new(),
        oracles: Vec::new(),
        hygiene: Vec::new(),
        gaps: Vec::new(),
        section_statements: BTreeMap::new(),
    };
    visitor.visit_block(&function.block);
    if observable_test_return {
        if let Some(syn::Stmt::Expr(expression, None)) = function.block.stmts.last() {
            if visitor.is_oracle_section(expression.span())
                && !is_unconditional_success(expression)
                && !visitor
                    .oracles
                    .iter()
                    .any(|oracle| oracle.span == source_span(expression.span()))
            {
                visitor.record_oracle(
                    OracleKind::Execution,
                    expression.span(),
                    expression.to_token_stream().to_string(),
                    expression_facts(expression),
                    ExprFacts::default(),
                    OracleFlags::default(),
                );
            }
        }
    }
    if markers.is_empty() {
        let outcome_oracles: Vec<_> = visitor
            .oracles
            .iter()
            .filter(|oracle| !oracle.conditional && oracle.kind != OracleKind::Interaction)
            .collect();
        let has_unobserved_call = visitor.calls.iter().any(|call| {
            !visitor
                .oracles
                .iter()
                .any(|oracle| source_span_contains(oracle.span, call.span))
        });
        if !outcome_oracles.is_empty()
            && has_unobserved_call
            && outcome_oracles.iter().all(|oracle| {
                let identifiers = oracle_identifiers(oracle);
                identifiers.is_disjoint(&visitor.act_outputs)
                    && identifiers.is_disjoint(&visitor.act_effects)
            })
        {
            visitor.gaps.push(AnalysisGap {
                code: "analysis.ambiguous-markerless-causality".into(),
                test: visitor.identity.clone(),
                message: "resolved oracle does not observe any inferred production output, but the test has no explicit Act boundary".into(),
                location: SourceLocation {
                    path: relative_path.into(),
                    span: outcome_oracles[0].span,
                    label: "markerless disconnected oracle".into(),
                },
            });
        }
    }
    if markers.is_empty()
        && !visitor.oracles.is_empty()
        && visitor
            .oracles
            .iter()
            .all(|oracle| oracle.kind == OracleKind::Execution)
    {
        let last_oracle_end = visitor
            .oracles
            .iter()
            .map(|oracle| oracle.span.end)
            .max_by_key(|position| (position.line, position.column));
        let unobserved_call = last_oracle_end.and_then(|last_oracle_end| {
            visitor.calls.iter().find(|call| {
                position_before(last_oracle_end, call.span.start)
                    && !visitor
                        .oracles
                        .iter()
                        .any(|oracle| source_span_contains(oracle.span, call.span))
            })
        });
        if let Some(call) = unobserved_call {
            visitor.gaps.push(AnalysisGap {
                code: "analysis.ambiguous-markerless-phase".into(),
                test: visitor.identity.clone(),
                message: format!(
                    "cannot prove whether call `{}` after the last execution check is an unobserved Act",
                    call.qualified_name
                ),
                location: SourceLocation {
                    path: relative_path.into(),
                    span: call.span,
                    label: "possible unobserved Act".into(),
                },
            });
        }
    }
    if markers
        .iter()
        .filter(|marker| marker.kind == MarkerKind::Act)
        .count()
        == 1
        && !visitor.oracles.iter().any(|oracle| {
            !oracle.conditional
                && oracle.kind != OracleKind::Interaction
                && oracle.section == Section::Assert
        })
    {
        let last_act_oracle_end = visitor
            .oracles
            .iter()
            .filter(|oracle| {
                !oracle.conditional
                    && oracle.kind != OracleKind::Interaction
                    && oracle.section == Section::Act
            })
            .map(|oracle| oracle.span.end)
            .max_by_key(|position| (position.line, position.column));
        let unobserved_call = last_act_oracle_end.and_then(|last_oracle_end| {
            visitor.calls.iter().find(|call| {
                call.section == Section::Act
                    && position_before(last_oracle_end, call.span.start)
                    && !visitor
                        .oracles
                        .iter()
                        .any(|oracle| source_span_contains(oracle.span, call.span))
            })
        });
        if let Some(call) = unobserved_call {
            visitor.gaps.push(AnalysisGap {
                code: "analysis.ambiguous-act-causality".into(),
                test: visitor.identity.clone(),
                message: format!(
                    "cannot prove whether call `{}` after the last Act execution check is unobserved production behavior",
                    call.qualified_name
                ),
                location: SourceLocation {
                    path: relative_path.into(),
                    span: call.span,
                    label: "possible unobserved Act call".into(),
                },
            });
        }
    }
    if markers
        .iter()
        .filter(|marker| marker.kind == MarkerKind::Act)
        .count()
        == 1
    {
        let observed_outputs: BTreeSet<_> = visitor
            .oracles
            .iter()
            .filter(|oracle| {
                !oracle.conditional
                    && oracle.kind != OracleKind::Interaction
                    && oracle.section == Section::Assert
            })
            .flat_map(oracle_identifiers)
            .filter(|identifier| visitor.act_outputs.contains(identifier))
            .collect();
        let last_observed_producer = observed_outputs
            .iter()
            .filter_map(|identifier| visitor.act_output_spans.get(identifier))
            .map(|span| span.end)
            .max_by_key(|position| (position.line, position.column));
        let later_unobserved_call = last_observed_producer.and_then(|producer_end| {
            visitor.calls.iter().find(|call| {
                call.section == Section::Act
                    && position_before(producer_end, call.span.start)
                    && call.receiver_identifiers.is_disjoint(&observed_outputs)
                    && call.argument_identifiers.is_disjoint(&observed_outputs)
                    && !visitor
                        .oracles
                        .iter()
                        .any(|oracle| source_span_contains(oracle.span, call.span))
            })
        });
        if let Some(call) = later_unobserved_call {
            visitor.gaps.push(AnalysisGap {
                code: "analysis.unobserved-later-act-call".into(),
                test: visitor.identity.clone(),
                message: format!(
                    "Act call `{}` occurs after the production value observed by the assertions, and its outcome is not observed",
                    call.qualified_name
                ),
                location: SourceLocation {
                    path: relative_path.into(),
                    span: call.span,
                    label: "later unobserved Act call".into(),
                },
            });
        }
    }
    let indirect_causality: Vec<_> = if markers
        .iter()
        .filter(|marker| marker.kind == MarkerKind::Act)
        .count()
        == 1
    {
        visitor
            .oracles
            .iter()
            .filter(|oracle| oracle.kind != OracleKind::Interaction)
            .filter(|oracle| !oracle.conditional && oracle.section == Section::Assert)
            .filter(|oracle| {
                let identifiers = oracle_identifiers(oracle);
                identifiers.is_disjoint(&visitor.act_outputs)
                    && identifiers.is_disjoint(&visitor.act_effects)
                    && identifiers.is_disjoint(&visitor.act_receivers)
                    && identifiers.is_disjoint(&visitor.act_shared_references)
            })
            .filter(|oracle| {
                oracle
                    .actual_calls
                    .iter()
                    .chain(&oracle.expected_calls)
                    .any(|call| !is_local_observer(call))
            })
            .map(|oracle| (oracle.span, oracle.text.clone()))
            .collect()
    } else {
        Vec::new()
    };
    for (span, oracle) in indirect_causality {
        visitor.gaps.push(AnalysisGap {
            code: "analysis.indirect-causality".into(),
            test: visitor.identity.clone(),
            message: format!(
                "cannot prove whether indirect observable `{oracle}` is connected to the Act"
            ),
            location: SourceLocation {
                path: relative_path.into(),
                span,
                label: "indirect observable".into(),
            },
        });
    }
    let receiver_effect_causality: Vec<_> = visitor
        .oracles
        .iter()
        .filter(|oracle| oracle.kind != OracleKind::Interaction)
        .filter(|oracle| !oracle.conditional && oracle.section == Section::Assert)
        .filter(|oracle| {
            let identifiers = oracle_identifiers(oracle);
            identifiers.is_disjoint(&visitor.act_outputs)
                && identifiers.is_disjoint(&visitor.act_effects)
                && !identifiers.is_disjoint(&visitor.act_receivers)
                && identifiers.is_disjoint(&visitor.act_shared_references)
        })
        .map(|oracle| (oracle.span, oracle.text.clone()))
        .collect();
    for (span, oracle) in receiver_effect_causality {
        visitor.gaps.push(AnalysisGap {
            code: "analysis.receiver-effect-causality".into(),
            test: visitor.identity.clone(),
            message: format!(
                "cannot prove whether receiver observed by `{oracle}` was mutated by the Act"
            ),
            location: SourceLocation {
                path: relative_path.into(),
                span,
                label: "unresolved receiver effect".into(),
            },
        });
    }
    let shared_reference_effect_causality: Vec<_> = visitor
        .oracles
        .iter()
        .filter(|oracle| oracle.kind != OracleKind::Interaction)
        .filter(|oracle| !oracle.conditional && oracle.section == Section::Assert)
        .filter(|oracle| {
            let identifiers = oracle_identifiers(oracle);
            identifiers.is_disjoint(&visitor.act_outputs)
                && identifiers.is_disjoint(&visitor.act_effects)
                && !identifiers.is_disjoint(&visitor.act_shared_references)
        })
        .map(|oracle| (oracle.span, oracle.text.clone()))
        .collect();
    for (span, oracle) in shared_reference_effect_causality {
        visitor.gaps.push(AnalysisGap {
            code: "analysis.shared-reference-effect-causality".into(),
            test: visitor.identity.clone(),
            message: format!(
                "cannot prove whether shared reference observed by `{oracle}` was mutated through interior mutability"
            ),
            location: SourceLocation {
                path: relative_path.into(),
                span,
                label: "unresolved shared-reference effect".into(),
            },
        });
    }

    TestCase {
        path: relative_path.to_string(),
        name,
        qualified_name,
        name_span,
        function_span,
        line_count: function_span
            .end
            .line
            .saturating_sub(function_span.start.line)
            + 1,
        markers: markers.clone(),
        section_statements: visitor.section_statements,
        ignored,
        ignore_span,
        should_panic,
        should_panic_expected,
        act_outputs: visitor.act_outputs,
        act_effects: visitor.act_effects,
        act_root_calls: visitor.act_root_calls,
        uncontrolled_clock_outputs: visitor.uncontrolled_clock_outputs,
        oracles: visitor.oracles,
        calls: visitor.calls,
        hygiene: visitor.hygiene,
        gaps: visitor.gaps,
    }
}

struct FactVisitor<'a> {
    relative_path: &'a str,
    identity: TestIdentity,
    markers: &'a [Marker],
    policy: &'a TestPolicyConfig,
    has_explicit_sections: bool,
    inside_oracle: bool,
    control_flow_depth: usize,
    act_outputs: BTreeSet<String>,
    act_output_spans: BTreeMap<String, SourceSpan>,
    act_effects: BTreeSet<String>,
    act_receivers: BTreeSet<String>,
    act_shared_references: BTreeSet<String>,
    act_root_calls: BTreeSet<String>,
    uncontrolled_clock_outputs: BTreeSet<String>,
    local_origins: BTreeMap<String, String>,
    local_callable_origins: BTreeMap<String, String>,
    bound_names: BTreeSet<String>,
    current_binding_identifiers: BTreeSet<String>,
    mutable_reference_aliases: BTreeMap<String, BTreeSet<String>>,
    observable_test_return: bool,
    virtual_time: bool,
    use_aliases: BTreeMap<String, String>,
    calls: Vec<CallFact>,
    oracles: Vec<OracleFact>,
    hygiene: Vec<HygieneFact>,
    gaps: Vec<AnalysisGap>,
    section_statements: BTreeMap<Section, usize>,
}

struct CallEffects {
    mutable_identifiers: BTreeSet<String>,
    shared_reference_identifiers: BTreeSet<String>,
}

#[derive(Default)]
struct OracleFlags {
    self_derived_candidate: bool,
    tautological: bool,
}

impl FactVisitor<'_> {
    fn section(&self, span: ProcSpan) -> Section {
        section_at(self.markers, span.start().line)
    }

    fn record_call(
        &mut self,
        name: String,
        qualified_name: String,
        span: ProcSpan,
        receiver_identifiers: BTreeSet<String>,
        argument_identifiers: BTreeSet<String>,
        effects: CallEffects,
    ) {
        let section = self.section(span);
        if !self.inside_oracle
            && (section == Section::Act
                || (!self.has_explicit_sections && section == Section::Body))
        {
            self.act_receivers
                .extend(receiver_identifiers.iter().cloned());
            self.act_effects.extend(effects.mutable_identifiers);
            self.act_shared_references
                .extend(effects.shared_reference_identifiers);
        }
        self.calls.push(CallFact {
            name,
            qualified_name,
            span: source_span(span),
            section,
            receiver_identifiers,
            argument_identifiers,
        });
    }

    fn record_hygiene(&mut self, kind: HygieneKind, call: &str, span: ProcSpan) {
        self.hygiene.push(HygieneFact {
            kind,
            call: call.to_string(),
            span: source_span(span),
            section: self.section(span),
            in_oracle: self.inside_oracle,
        });
    }

    fn record_oracle(
        &mut self,
        kind: OracleKind,
        span: ProcSpan,
        text: String,
        actual: ExprFacts,
        expected: ExprFacts,
        flags: OracleFlags,
    ) {
        let actual_root_call = self.resolved_root_call(&actual);
        let expected_root_call = self.resolved_root_call(&expected);
        let conditional = self.control_flow_depth > 0;
        if conditional {
            self.gap(
                "analysis.conditional-oracle",
                "cannot prove that this conditional or nested oracle executes",
                span,
            );
        }
        self.oracles.push(OracleFact {
            kind,
            span: source_span(span),
            section: self.section(span),
            text,
            actual_identifiers: actual.identifiers,
            expected_identifiers: expected.identifiers,
            actual_calls: actual.calls,
            expected_calls: expected.calls,
            produced_identifiers: self.current_binding_identifiers.clone(),
            actual_root_call,
            expected_root_call,
            self_derived_candidate: flags.self_derived_candidate,
            conditional,
            tautological: flags.tautological,
        });
    }

    fn resolved_root_call(&self, facts: &ExprFacts) -> Option<String> {
        let local_origin = (facts.identifiers.len() == 1)
            .then(|| facts.identifiers.first())
            .flatten()
            .and_then(|identifier| self.local_origins.get(identifier))
            .cloned();
        if local_origin.is_some()
            && !facts.calls.is_empty()
            && facts.calls.iter().all(|call| {
                matches!(
                    call.rsplit("::").next().unwrap_or(call),
                    "as_ref" | "borrow" | "clone" | "to_owned"
                )
            })
        {
            return local_origin;
        }
        facts.root_call.clone().or(local_origin)
    }

    fn effect_identifiers_from_expression(&self, expression: &Expr) -> BTreeSet<String> {
        let mut identifiers = mutable_reference_identifiers(expression);
        if let Expr::Path(path) = expression {
            if let Some(identifier) = local_identifier(&path.path) {
                if let Some(aliased) = self.mutable_reference_aliases.get(&identifier) {
                    identifiers.extend(aliased.iter().cloned());
                }
            }
        }
        let aliases: Vec<_> = identifiers
            .iter()
            .filter_map(|identifier| self.mutable_reference_aliases.get(identifier))
            .flatten()
            .cloned()
            .collect();
        identifiers.extend(aliases);
        identifiers
    }

    fn effect_identifiers_for_arguments<'a>(
        &self,
        expressions: impl Iterator<Item = &'a Expr>,
    ) -> BTreeSet<String> {
        expressions
            .flat_map(|expression| self.effect_identifiers_from_expression(expression))
            .collect()
    }

    fn resolve_imported_name(&self, name: &str) -> String {
        let mut segments = name.split("::");
        let Some(first) = segments.next() else {
            return name.to_string();
        };
        if let Some(resolved) = self.local_callable_origins.get(first) {
            let remainder = segments.collect::<Vec<_>>().join("::");
            return if remainder.is_empty() {
                resolved.clone()
            } else {
                format!("{resolved}::{remainder}")
            };
        }
        if self.bound_names.contains(first) {
            return name.to_string();
        }
        resolve_alias(name, &self.use_aliases)
    }

    fn gap(&mut self, code: &str, message: impl Into<String>, span: ProcSpan) {
        self.gaps.push(AnalysisGap {
            code: code.to_string(),
            test: self.identity.clone(),
            message: message.into(),
            location: SourceLocation {
                path: self.relative_path.to_string(),
                span: source_span(span),
                label: "partially analyzed syntax".into(),
            },
        });
    }
}

impl<'ast> Visit<'ast> for FactVisitor<'_> {
    fn visit_block(&mut self, block: &'ast syn::Block) {
        let previous_aliases = self.use_aliases.clone();
        let previous_bindings = self.bound_names.clone();
        let previous_callable_origins = self.local_callable_origins.clone();
        for statement in &block.stmts {
            if let syn::Stmt::Item(item) = statement {
                if let Item::Use(import) = item {
                    collect_use_tree(&import.tree, &[], &mut self.use_aliases);
                } else if let Item::Type(alias) = item {
                    if !collect_type_alias(alias, &mut self.use_aliases) {
                        self.bound_names.insert(alias.ident.to_string());
                    }
                } else if let Some(identifier) = item_binding_name(item) {
                    self.bound_names.insert(identifier);
                }
            }
        }
        visit::visit_block(self, block);
        self.use_aliases = previous_aliases;
        self.bound_names = previous_bindings;
        self.local_callable_origins = previous_callable_origins;
    }

    fn visit_stmt(&mut self, statement: &'ast syn::Stmt) {
        let section = self.section(statement.span());
        *self.section_statements.entry(section).or_default() += 1;
        visit::visit_stmt(self, statement);
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        let section = self.section(local.span());
        let mut bound_identifiers = BTreeSet::new();
        identifiers_from_pattern(&local.pat, &mut bound_identifiers);
        let mut callable_binding = None;
        if let Some(init) = &local.init {
            let facts = expression_facts(&init.expr);
            if bound_identifiers
                .iter()
                .any(|identifier| self.bound_names.contains(identifier))
            {
                self.gap(
                    "analysis.shadowed-binding",
                    "local shadowing makes identifier-only causality ambiguous",
                    local.span(),
                );
            }
            if let Some(callable) = callable_path_name(&init.expr) {
                callable_binding = Some(self.resolve_imported_name(&callable));
            }
            let alias_sources = self.effect_identifiers_from_expression(&init.expr);
            if !alias_sources.is_empty() {
                for identifier in &bound_identifiers {
                    self.mutable_reference_aliases
                        .insert(identifier.clone(), alias_sources.clone());
                }
            }
            let origin = facts.root_call.clone().or_else(|| {
                (facts.identifiers.len() == 1)
                    .then(|| facts.identifiers.first())
                    .flatten()
                    .and_then(|identifier| self.local_origins.get(identifier))
                    .cloned()
            });
            if let Some(origin) = origin {
                for identifier in &bound_identifiers {
                    self.local_origins
                        .insert(identifier.clone(), origin.clone());
                }
            }
            if facts
                .calls
                .iter()
                .any(|call| is_clock_call(&self.resolve_imported_name(call)))
                || !facts
                    .identifiers
                    .is_disjoint(&self.uncontrolled_clock_outputs)
            {
                self.uncontrolled_clock_outputs
                    .extend(bound_identifiers.iter().cloned());
            }
            let derived_from_act = !facts.identifiers.is_disjoint(&self.act_outputs)
                || !facts.identifiers.is_disjoint(&self.act_effects);
            let computation = is_nontrivial_act_expression(&init.expr);
            if section == Section::Act && computation && facts.calls.is_empty() && !derived_from_act
            {
                self.gap(
                    "analysis.unresolved-act-computation",
                    "cannot prove whether this non-call expression is the production behavior or Act-local plumbing",
                    init.expr.span(),
                );
            }
            if (section == Section::Act && (!facts.calls.is_empty() || derived_from_act))
                || (!self.has_explicit_sections && !facts.calls.is_empty())
            {
                self.act_outputs.extend(bound_identifiers.iter().cloned());
                if section == Section::Act {
                    let span = source_span(init.expr.span());
                    for identifier in &bound_identifiers {
                        self.act_output_spans.insert(identifier.clone(), span);
                    }
                }
                if let Some(root_call) = facts.root_call {
                    self.act_root_calls.insert(root_call);
                }
            } else if !facts.identifiers.is_disjoint(&self.act_outputs) {
                identifiers_from_pattern(&local.pat, &mut self.act_outputs);
            }
        }
        let previous_bindings = std::mem::replace(
            &mut self.current_binding_identifiers,
            bound_identifiers.clone(),
        );
        visit::visit_local(self, local);
        self.current_binding_identifiers = previous_bindings;
        if let Some(resolved) = callable_binding {
            for identifier in &bound_identifiers {
                self.local_callable_origins
                    .insert(identifier.clone(), resolved.clone());
            }
        }
        self.bound_names.extend(bound_identifiers);
    }

    fn visit_expr_for_loop(&mut self, expression: &'ast syn::ExprForLoop) {
        let facts = expression_facts(&expression.expr);
        if !facts.identifiers.is_disjoint(&self.act_outputs) {
            identifiers_from_pattern(&expression.pat, &mut self.act_outputs);
            self.gap(
                "analysis.pattern-causality",
                "cannot prove causality through a loop pattern binding",
                expression.pat.span(),
            );
        }
        self.control_flow_depth += 1;
        self.visit_expr(&expression.expr);
        let previous_bindings = self.bound_names.clone();
        identifiers_from_pattern(&expression.pat, &mut self.bound_names);
        self.visit_block(&expression.body);
        self.bound_names = previous_bindings;
        self.control_flow_depth -= 1;
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        let previous_bindings = self.bound_names.clone();
        identifiers_from_pattern(&arm.pat, &mut self.bound_names);
        visit::visit_arm(self, arm);
        self.bound_names = previous_bindings;
    }

    fn visit_expr_assign(&mut self, expression: &'ast syn::ExprAssign) {
        let section = self.section(expression.span());
        let left = expression_facts(&expression.left);
        let right = expression_facts(&expression.right);
        if section == Section::Act {
            let derived_from_act = !right.identifiers.is_disjoint(&self.act_outputs)
                || !right.identifiers.is_disjoint(&self.act_effects);
            if is_nontrivial_act_expression(&expression.right)
                && right.calls.is_empty()
                && !derived_from_act
            {
                self.gap(
                    "analysis.unresolved-act-computation",
                    "cannot prove whether this non-call expression is the production behavior or Act-local plumbing",
                    expression.right.span(),
                );
            }
            if !right.calls.is_empty() || derived_from_act {
                self.act_outputs.extend(left.identifiers.iter().cloned());
                let span = source_span(expression.right.span());
                for identifier in &left.identifiers {
                    self.act_output_spans.insert(identifier.clone(), span);
                }
                if let Some(root_call) = &right.root_call {
                    self.act_root_calls.insert(root_call.clone());
                }
            }
        } else if !right.identifiers.is_disjoint(&self.act_outputs) {
            self.act_outputs.extend(left.identifiers.iter().cloned());
        }
        if right
            .calls
            .iter()
            .any(|call| is_clock_call(&self.resolve_imported_name(call)))
            || !right
                .identifiers
                .is_disjoint(&self.uncontrolled_clock_outputs)
        {
            self.uncontrolled_clock_outputs
                .extend(left.identifiers.iter().cloned());
        }
        if let Some(origin) = right.root_call.or_else(|| {
            (right.identifiers.len() == 1)
                .then(|| right.identifiers.first())
                .flatten()
                .and_then(|identifier| self.local_origins.get(identifier))
                .cloned()
        }) {
            for identifier in &left.identifiers {
                self.local_origins
                    .insert(identifier.clone(), origin.clone());
            }
        }
        let previous_bindings =
            std::mem::replace(&mut self.current_binding_identifiers, left.identifiers);
        visit::visit_expr_assign(self, expression);
        self.current_binding_identifiers = previous_bindings;
    }

    fn visit_expr_binary(&mut self, expression: &'ast syn::ExprBinary) {
        if self.section(expression.span()) == Section::Act && is_compound_assignment(&expression.op)
        {
            let left = expression_facts(&expression.left);
            self.act_effects.extend(left.identifiers);
            self.gap(
                "analysis.unresolved-act-computation",
                "cannot prove whether this compound assignment exercises production behavior or test-local arithmetic",
                expression.span(),
            );
        }
        visit::visit_expr_binary(self, expression);
    }

    fn visit_expr_match(&mut self, expression: &'ast syn::ExprMatch) {
        if self.is_oracle_section(expression.span())
            && expression.arms.len() > 1
            && expression.arms.iter().any(|arm| contains_panic(&arm.body))
        {
            let kind = classify_assert_match(expression);
            self.record_oracle(
                kind,
                expression.span(),
                expression.to_token_stream().to_string(),
                expression_facts(&expression.expr),
                ExprFacts::default(),
                OracleFlags::default(),
            );
        }
        self.control_flow_depth += 1;
        visit::visit_expr_match(self, expression);
        self.control_flow_depth -= 1;
    }

    fn visit_expr_if(&mut self, expression: &'ast syn::ExprIf) {
        if self.section(expression.span()) == Section::Assert
            && (contains_panic_block(&expression.then_branch)
                || expression
                    .else_branch
                    .as_ref()
                    .is_some_and(|(_, branch)| contains_panic(branch)))
        {
            self.record_oracle(
                OracleKind::Outcome,
                expression.span(),
                expression.to_token_stream().to_string(),
                expression_facts(&expression.cond),
                ExprFacts::default(),
                OracleFlags::default(),
            );
        }
        if let Expr::Let(pattern) = &*expression.cond {
            let source = expression_facts(&pattern.expr);
            if !source.identifiers.is_disjoint(&self.act_outputs)
                || !source.identifiers.is_disjoint(&self.act_effects)
            {
                self.gap(
                    "analysis.pattern-causality",
                    "cannot prove causality through a conditional pattern binding",
                    pattern.span(),
                );
            }
        }
        self.control_flow_depth += 1;
        self.visit_expr(&expression.cond);
        let previous_bindings = self.bound_names.clone();
        if let Expr::Let(pattern) = &*expression.cond {
            identifiers_from_pattern(&pattern.pat, &mut self.bound_names);
        }
        self.visit_block(&expression.then_branch);
        self.bound_names = previous_bindings;
        if let Some((_, branch)) = &expression.else_branch {
            self.visit_expr(branch);
        }
        self.control_flow_depth -= 1;
    }

    fn visit_expr_closure(&mut self, expression: &'ast syn::ExprClosure) {
        self.control_flow_depth += 1;
        let previous_bindings = self.bound_names.clone();
        for input in &expression.inputs {
            identifiers_from_pattern(input, &mut self.bound_names);
        }
        visit::visit_expr_closure(self, expression);
        self.bound_names = previous_bindings;
        self.control_flow_depth -= 1;
    }

    fn visit_expr_async(&mut self, expression: &'ast syn::ExprAsync) {
        self.control_flow_depth += 1;
        visit::visit_expr_async(self, expression);
        self.control_flow_depth -= 1;
    }

    fn visit_expr_loop(&mut self, expression: &'ast syn::ExprLoop) {
        self.control_flow_depth += 1;
        visit::visit_expr_loop(self, expression);
        self.control_flow_depth -= 1;
    }

    fn visit_expr_while(&mut self, expression: &'ast syn::ExprWhile) {
        self.control_flow_depth += 1;
        self.visit_expr(&expression.cond);
        let previous_bindings = self.bound_names.clone();
        if let Expr::Let(pattern) = &*expression.cond {
            identifiers_from_pattern(&pattern.pat, &mut self.bound_names);
        }
        self.visit_block(&expression.body);
        self.bound_names = previous_bindings;
        self.control_flow_depth -= 1;
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        let source_name = callable_name(&call.func).unwrap_or_else(|| "<dynamic-call>".into());
        let qualified_name = self.resolve_imported_name(&source_name);
        let name = source_name
            .rsplit("::")
            .next()
            .unwrap_or(&source_name)
            .to_string();
        let arguments = facts_for_expressions(call.args.iter());
        let is_interaction = self.policy.matches_mock_interaction(&qualified_name);
        let is_assertion =
            self.policy.matches_assertion_function(&qualified_name) || is_interaction;
        if qualified_name == source_name
            && self.use_aliases.contains_key(GLOB_IMPORT_SENTINEL)
            && !self
                .bound_names
                .contains(source_name.split("::").next().unwrap_or(&source_name))
            && may_be_hygiene_call_from_glob(&source_name)
        {
            self.gap(
                "analysis.unresolved-glob-import",
                format!(
                    "cannot determine whether `{source_name}` resolves to a nondeterministic API imported through a glob"
                ),
                call.span(),
            );
        }
        if is_assertion {
            self.record_oracle(
                if is_interaction {
                    OracleKind::Interaction
                } else {
                    OracleKind::Outcome
                },
                call.span(),
                call.to_token_stream().to_string(),
                arguments,
                ExprFacts::default(),
                OracleFlags::default(),
            );
        } else if !self.inside_oracle && name.starts_with("should_") {
            self.gap(
                "analysis.unresolved-scenario-delegate",
                format!("scenario-like delegate `{source_name}` cannot be inspected here"),
                call.span(),
            );
        } else if !self.inside_oracle
            && (name.starts_with("assert_")
                || (self.section(call.span()) == Section::Assert
                    && looks_like_assertion_helper(&name)))
        {
            self.gap(
                "analysis.unresolved-assertion-helper",
                format!("assertion-like helper `{qualified_name}` is not configured"),
                call.span(),
            );
        }
        if !self.inside_oracle && self.control_flow_depth == 0 {
            if qualified_name == "tokio::time::pause" {
                self.virtual_time = true;
            } else if qualified_name == "tokio::time::resume" {
                self.virtual_time = false;
            }
        }
        self.detect_hygiene_call(&qualified_name, &call.args, call.span());
        let effect_identifiers = self.effect_identifiers_for_arguments(call.args.iter());
        let shared_reference_identifiers =
            shared_reference_identifiers_for_arguments(call.args.iter());
        let previous = self.inside_oracle;
        if is_assertion {
            self.inside_oracle = true;
        }
        self.record_call(
            name,
            qualified_name,
            call.span(),
            BTreeSet::new(),
            facts_for_expressions(call.args.iter()).identifiers,
            CallEffects {
                mutable_identifiers: effect_identifiers,
                shared_reference_identifiers,
            },
        );
        visit::visit_expr_call(self, call);
        self.inside_oracle = previous;
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        let name = call.method.to_string();
        let receiver = expression_facts(&call.receiver);
        let arguments = facts_for_expressions(call.args.iter());
        let is_interaction = self.policy.matches_mock_interaction(&name);
        let is_assertion = self.policy.matches_assertion_function(&name) || is_interaction;
        if is_assertion {
            let mut actual = receiver.clone();
            actual.merge(arguments.clone());
            self.record_oracle(
                if is_interaction {
                    OracleKind::Interaction
                } else {
                    OracleKind::Outcome
                },
                call.span(),
                call.to_token_stream().to_string(),
                actual,
                ExprFacts::default(),
                OracleFlags::default(),
            );
        } else if !self.inside_oracle
            && (name.starts_with("assert_")
                || (self.section(call.span()) == Section::Assert
                    && looks_like_assertion_helper(&name)))
        {
            self.gap(
                "analysis.unresolved-assertion-helper",
                format!("assertion-like method `{name}` is not configured"),
                call.span(),
            );
        } else if matches!(name.as_str(), "unwrap" | "expect")
            && self.is_oracle_section(call.span())
        {
            self.record_oracle(
                OracleKind::Execution,
                call.span(),
                call.to_token_stream().to_string(),
                receiver.clone(),
                ExprFacts::default(),
                OracleFlags::default(),
            );
        } else if matches!(name.as_str(), "unwrap_err" | "expect_err")
            && self.is_oracle_section(call.span())
        {
            self.record_oracle(
                OracleKind::BroadError,
                call.span(),
                call.to_token_stream().to_string(),
                receiver.clone(),
                ExprFacts::default(),
                OracleFlags::default(),
            );
        }
        let mut effect_identifiers = self.effect_identifiers_from_expression(&call.receiver);
        effect_identifiers.extend(self.effect_identifiers_for_arguments(call.args.iter()));
        let shared_reference_identifiers =
            shared_reference_identifiers_for_arguments(call.args.iter());
        let previous = self.inside_oracle;
        if is_assertion {
            self.inside_oracle = true;
        }
        self.record_call(
            name.clone(),
            name,
            call.span(),
            direct_receiver_identifiers(&call.receiver),
            arguments.identifiers,
            CallEffects {
                mutable_identifiers: effect_identifiers,
                shared_reference_identifiers,
            },
        );
        visit::visit_expr_method_call(self, call);
        self.inside_oracle = previous;
    }

    fn visit_macro(&mut self, expression: &'ast syn::Macro) {
        let name = path_name(&expression.path);
        if self.inside_oracle
            && matches!(
                name.rsplit("::").next().unwrap_or(&name),
                "format" | "format_args"
            )
            && has_implicit_format_capture(expression)
        {
            self.gap(
                "analysis.implicit-format-capture",
                "cannot attribute identifiers captured implicitly by the format string",
                expression.span(),
            );
        }
        if self.policy.matches_assertion_macro(&name) {
            self.analyze_assertion_macro(expression, &name);
            return;
        }
        if (self.section(expression.span()) == Section::Assert
            || name
                .rsplit("::")
                .next()
                .is_some_and(|name| name.starts_with("assert")))
            && !known_non_assertion_macro(&name)
        {
            self.gap(
                "analysis.unsupported-macro",
                format!("custom macro `{name}!` is not configured as an assertion"),
                expression.span(),
            );
        }
        visit::visit_macro(self, expression);
    }

    fn visit_expr_try(&mut self, expression: &'ast ExprTry) {
        let section = self.section(expression.span());
        if section == Section::Act || section == Section::Assert || !self.has_explicit_sections {
            self.record_oracle(
                OracleKind::Execution,
                expression.span(),
                expression.to_token_stream().to_string(),
                expression_facts(&expression.expr),
                ExprFacts::default(),
                OracleFlags::default(),
            );
        }
        visit::visit_expr_try(self, expression);
    }

    fn visit_expr_return(&mut self, expression: &'ast syn::ExprReturn) {
        if self.observable_test_return {
            if let Some(value) = &expression.expr {
                if self.is_oracle_section(expression.span()) && !is_unconditional_success(value) {
                    self.record_oracle(
                        OracleKind::Execution,
                        expression.span(),
                        expression.to_token_stream().to_string(),
                        expression_facts(value),
                        ExprFacts::default(),
                        OracleFlags::default(),
                    );
                }
            }
        }
        visit::visit_expr_return(self, expression);
    }
}

impl FactVisitor<'_> {
    fn is_oracle_section(&self, span: ProcSpan) -> bool {
        matches!(self.section(span), Section::Act | Section::Assert)
            || (!self.has_explicit_sections && self.section(span) == Section::Body)
    }

    fn analyze_assertion_macro(&mut self, expression: &syn::Macro, name: &str) {
        let text = expression.to_token_stream().to_string();
        let parsed = syn::parse2::<ExprList>(expression.tokens.clone());
        let (actual, expected, self_derived_candidate, tautological, kind) = match parsed {
            Ok(arguments) if !arguments.0.is_empty() => {
                let first = &arguments.0[0];
                let actual = expression_facts(first);
                let final_name = name.rsplit("::").next().unwrap_or(name);
                let assertion_matches = final_name == "assert_matches";
                let expected = if assertion_matches {
                    // The second argument is a pattern. Names in it introduce bindings rather
                    // than refer to local values, so they cannot establish oracle causality.
                    ExprFacts::default()
                } else {
                    arguments
                        .0
                        .get(1)
                        .map_or_else(ExprFacts::default, expression_facts)
                };
                let condition_macro =
                    matches!(final_name, "assert" | "debug_assert" | "prop_assert");
                let equality_macro = matches!(
                    final_name,
                    "assert_eq" | "debug_assert_eq" | "prop_assert_eq"
                );
                let identical_equality = equality_macro
                    && arguments.0.get(1).is_some_and(|second| {
                        normalized_tokens(first) == normalized_tokens(second)
                    });
                let self_comparison = identical_equality
                    && is_provably_reflexive_expression(first)
                    && arguments
                        .0
                        .get(1)
                        .is_some_and(is_provably_reflexive_expression);
                let direct_self_comparison = condition_macro && is_self_comparison(first);
                if (identical_equality && is_simple_value_expression(first) && !self_comparison)
                    || (condition_macro && is_possible_self_comparison(first))
                {
                    self.gap(
                        "analysis.possible-self-comparison",
                        "the assertion repeats an expression, but its equality semantics cannot be proven tautological",
                        expression.span(),
                    );
                }
                let always_true = condition_macro && expression_is_always_true(first);
                let broad_error = (condition_macro && contains_broad_error_check(first))
                    || (equality_macro
                        && arguments.0.get(1).is_some_and(|second| {
                            (contains_broad_error_check(first) && is_true_literal(second))
                                || (is_true_literal(first) && contains_broad_error_check(second))
                        }))
                    || (assertion_matches
                        && arguments
                            .0
                            .get(1)
                            .is_some_and(is_broad_err_pattern_expression));
                (
                    actual,
                    expected,
                    equality_macro,
                    self_comparison || direct_self_comparison || always_true,
                    if broad_error {
                        OracleKind::BroadError
                    } else {
                        OracleKind::Outcome
                    },
                )
            }
            _ => {
                self.gap(
                    "analysis.partially-parsed-assertion",
                    format!("arguments to `{name}!` could not be fully parsed"),
                    expression.span(),
                );
                (
                    facts_from_tokens(&expression.tokens),
                    ExprFacts::default(),
                    false,
                    false,
                    OracleKind::Outcome,
                )
            }
        };
        self.record_oracle(
            kind,
            expression.span(),
            text,
            actual,
            expected,
            OracleFlags {
                self_derived_candidate,
                tautological,
            },
        );

        let previous = self.inside_oracle;
        self.inside_oracle = true;
        if let Ok(arguments) = syn::parse2::<ExprList>(expression.tokens.clone()) {
            for argument in &arguments.0 {
                self.visit_expr(argument);
            }
        }
        self.inside_oracle = previous;
    }

    fn detect_hygiene_call(
        &mut self,
        qualified_name: &str,
        arguments: &Punctuated<Expr, Token![,]>,
        span: ProcSpan,
    ) {
        if is_fixed_sleep_call(qualified_name)
            && !(self.virtual_time && qualified_name == "tokio::time::sleep")
            && !self.policy.is_controlled_time(qualified_name)
            && fixed_duration(arguments)
        {
            self.record_hygiene(HygieneKind::FixedSleep, qualified_name, span);
        }
        if is_clock_call(qualified_name)
            && !(self.virtual_time && qualified_name == "tokio::time::Instant::now")
            && !self.policy.is_controlled_time(qualified_name)
        {
            self.record_hygiene(HygieneKind::UncontrolledClock, qualified_name, span);
        }
        if is_random_call(qualified_name) && !self.policy.is_controlled_random(qualified_name) {
            self.record_hygiene(HygieneKind::UncontrolledRandom, qualified_name, span);
        }
        if is_environment_mutation(qualified_name)
            && !self.policy.is_controlled_environment(qualified_name)
        {
            self.record_hygiene(HygieneKind::ProcessEnvironment, qualified_name, span);
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ExprFacts {
    identifiers: BTreeSet<String>,
    calls: BTreeSet<String>,
    root_call: Option<String>,
}

impl ExprFacts {
    fn merge(&mut self, other: Self) {
        self.identifiers.extend(other.identifiers);
        self.calls.extend(other.calls);
        if self.root_call.is_none() {
            self.root_call = other.root_call;
        }
    }
}

#[derive(Default)]
struct ExprFactsVisitor {
    facts: ExprFacts,
}

#[derive(Default)]
struct PanicVisitor {
    found: bool,
}

#[derive(Default)]
struct BroadErrorVisitor {
    found: bool,
}

impl<'ast> Visit<'ast> for PanicVisitor {
    fn visit_macro(&mut self, expression: &'ast syn::Macro) {
        if path_name(&expression.path)
            .rsplit("::")
            .next()
            .is_some_and(|name| matches!(name, "panic" | "unreachable"))
        {
            self.found = true;
            return;
        }
        visit::visit_macro(self, expression);
    }
}

impl<'ast> Visit<'ast> for BroadErrorVisitor {
    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if call.method == "is_err" {
            self.found = true;
            return;
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, expression: &'ast syn::Macro) {
        if path_name(&expression.path)
            .rsplit("::")
            .next()
            .is_some_and(|name| name == "matches")
        {
            self.found |= syn::parse2::<ExprList>(expression.tokens.clone())
                .ok()
                .and_then(|arguments| arguments.0.get(1).cloned())
                .is_some_and(|pattern| is_broad_err_pattern_expression(&pattern));
            return;
        }
        visit::visit_macro(self, expression);
    }
}

impl<'ast> Visit<'ast> for ExprFactsVisitor {
    fn visit_expr_path(&mut self, path: &'ast ExprPath) {
        if let Some(identifier) = local_identifier(&path.path) {
            self.facts.identifiers.insert(identifier);
        }
        visit::visit_expr_path(self, path);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Some(name) = callable_name(&call.func) {
            self.facts.calls.insert(name);
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        self.facts.calls.insert(call.method.to_string());
        visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, expression: &'ast syn::Macro) {
        let name = path_name(&expression.path);
        let final_name = name.rsplit("::").next().unwrap_or(&name);
        if final_name == "matches" {
            if let Ok(arguments) = syn::parse2::<ExprList>(expression.tokens.clone()) {
                if let Some(scrutinee) = arguments.0.first() {
                    self.visit_expr(scrutinee);
                }
            }
            return;
        }
        if matches!(final_name, "format" | "format_args" | "vec") {
            collect_token_identifiers(expression.tokens.clone(), &mut self.facts.identifiers);
            return;
        }
        visit::visit_macro(self, expression);
    }
}

fn expression_facts(expression: &Expr) -> ExprFacts {
    let mut visitor = ExprFactsVisitor::default();
    visitor.visit_expr(expression);
    visitor.facts.root_call = root_callable(expression);
    visitor.facts
}

fn is_nontrivial_act_expression(expression: &Expr) -> bool {
    match expression {
        Expr::Array(_)
        | Expr::Async(_)
        | Expr::Await(_)
        | Expr::Binary(_)
        | Expr::Call(_)
        | Expr::Cast(_)
        | Expr::Index(_)
        | Expr::Macro(_)
        | Expr::Match(_)
        | Expr::MethodCall(_)
        | Expr::Struct(_)
        | Expr::Try(_)
        | Expr::TryBlock(_)
        | Expr::Tuple(_)
        | Expr::Unary(_) => true,
        Expr::Group(group) => is_nontrivial_act_expression(&group.expr),
        Expr::Paren(parenthesized) => is_nontrivial_act_expression(&parenthesized.expr),
        _ => false,
    }
}

fn is_compound_assignment(operator: &syn::BinOp) -> bool {
    matches!(
        operator,
        syn::BinOp::AddAssign(_)
            | syn::BinOp::SubAssign(_)
            | syn::BinOp::MulAssign(_)
            | syn::BinOp::DivAssign(_)
            | syn::BinOp::RemAssign(_)
            | syn::BinOp::BitXorAssign(_)
            | syn::BinOp::BitAndAssign(_)
            | syn::BinOp::BitOrAssign(_)
            | syn::BinOp::ShlAssign(_)
            | syn::BinOp::ShrAssign(_)
    )
}

fn contains_panic(expression: &Expr) -> bool {
    let mut visitor = PanicVisitor::default();
    visitor.visit_expr(expression);
    visitor.found
}

fn contains_panic_block(block: &syn::Block) -> bool {
    let mut visitor = PanicVisitor::default();
    visitor.visit_block(block);
    visitor.found
}

fn facts_for_expressions<'a>(expressions: impl Iterator<Item = &'a Expr>) -> ExprFacts {
    let mut facts = ExprFacts::default();
    for expression in expressions {
        facts.merge(expression_facts(expression));
    }
    facts
}

fn mutable_reference_identifiers(expression: &Expr) -> BTreeSet<String> {
    if let Expr::Reference(reference) = expression {
        if reference.mutability.is_some() {
            return expression_facts(&reference.expr).identifiers;
        }
    }
    BTreeSet::new()
}

fn shared_reference_identifiers(expression: &Expr) -> BTreeSet<String> {
    match expression {
        Expr::Reference(reference) if reference.mutability.is_none() => {
            expression_facts(&reference.expr).identifiers
        }
        Expr::Group(group) => shared_reference_identifiers(&group.expr),
        Expr::Paren(parenthesized) => shared_reference_identifiers(&parenthesized.expr),
        _ => BTreeSet::new(),
    }
}

fn shared_reference_identifiers_for_arguments<'a>(
    expressions: impl Iterator<Item = &'a Expr>,
) -> BTreeSet<String> {
    expressions.flat_map(shared_reference_identifiers).collect()
}

fn direct_receiver_identifiers(expression: &Expr) -> BTreeSet<String> {
    match expression {
        Expr::Path(_) | Expr::Field(_) | Expr::Index(_) => expression_facts(expression).identifiers,
        Expr::Group(group) => direct_receiver_identifiers(&group.expr),
        Expr::Paren(parenthesized) => direct_receiver_identifiers(&parenthesized.expr),
        Expr::Reference(reference) => direct_receiver_identifiers(&reference.expr),
        _ => BTreeSet::new(),
    }
}

fn oracle_identifiers(oracle: &OracleFact) -> BTreeSet<String> {
    oracle
        .actual_identifiers
        .union(&oracle.expected_identifiers)
        .cloned()
        .collect()
}

fn facts_from_tokens(tokens: &TokenStream) -> ExprFacts {
    let mut identifiers = BTreeSet::new();
    collect_token_identifiers(tokens.clone(), &mut identifiers);
    ExprFacts {
        identifiers,
        calls: BTreeSet::new(),
        root_call: None,
    }
}

fn collect_token_identifiers(tokens: TokenStream, identifiers: &mut BTreeSet<String>) {
    for token in tokens {
        match token {
            TokenTree::Ident(identifier) => {
                let value = identifier.to_string();
                if is_local_name(&value) {
                    identifiers.insert(value);
                }
            }
            TokenTree::Group(group) => collect_token_identifiers(group.stream(), identifiers),
            _ => {}
        }
    }
}

fn has_implicit_format_capture(expression: &syn::Macro) -> bool {
    let Ok(arguments) = syn::parse2::<ExprList>(expression.tokens.clone()) else {
        return false;
    };
    let Some(Expr::Lit(ExprLit {
        lit: Lit::Str(format),
        ..
    })) = arguments.0.first()
    else {
        return false;
    };
    let value = format.value();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'{') {
            index += 2;
            continue;
        }
        let Some(relative_end) = value[index + 1..].find('}') else {
            return false;
        };
        let end = index + 1 + relative_end;
        let field = value[index + 1..end].trim_start();
        if field
            .chars()
            .next()
            .is_some_and(|character| character == '_' || character.is_alphabetic())
        {
            return true;
        }
        index = end + 1;
    }
    false
}

struct ExprList(Punctuated<Expr, Token![,]>);

impl Parse for ExprList {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        Ok(Self(Punctuated::parse_terminated(input)?))
    }
}

fn identifiers_from_pattern(pattern: &Pat, identifiers: &mut BTreeSet<String>) {
    match pattern {
        Pat::Ident(identifier) => {
            identifiers.insert(identifier.ident.to_string());
            if let Some((_, subpattern)) = &identifier.subpat {
                identifiers_from_pattern(subpattern, identifiers);
            }
        }
        Pat::Tuple(tuple) => {
            for element in &tuple.elems {
                identifiers_from_pattern(element, identifiers);
            }
        }
        Pat::TupleStruct(tuple) => {
            for element in &tuple.elems {
                identifiers_from_pattern(element, identifiers);
            }
        }
        Pat::Struct(structure) => {
            for field in &structure.fields {
                identifiers_from_pattern(&field.pat, identifiers);
            }
        }
        Pat::Reference(reference) => identifiers_from_pattern(&reference.pat, identifiers),
        Pat::Slice(slice) => {
            for element in &slice.elems {
                identifiers_from_pattern(element, identifiers);
            }
        }
        Pat::Type(typed) => identifiers_from_pattern(&typed.pat, identifiers),
        Pat::Paren(parenthesized) => identifiers_from_pattern(&parenthesized.pat, identifiers),
        _ => {}
    }
}

fn local_identifier(path: &SynPath) -> Option<String> {
    if path.leading_colon.is_some() || path.segments.len() != 1 {
        return None;
    }
    let value = path.segments.first()?.ident.to_string();
    is_local_name(&value).then_some(value)
}

fn is_local_name(value: &str) -> bool {
    value == "self"
        || value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character == '_')
}

fn callable_name(expression: &Expr) -> Option<String> {
    if let Expr::Path(path) = expression {
        Some(path_name(&path.path))
    } else {
        None
    }
}

fn callable_path_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Path(path) if path.qself.is_none() => Some(path_name(&path.path)),
        Expr::Group(group) => callable_path_name(&group.expr),
        Expr::Paren(parenthesized) => callable_path_name(&parenthesized.expr),
        _ => None,
    }
}

fn root_callable(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Call(_) | Expr::MethodCall(_) => Some(normalized_tokens(expression)),
        Expr::Await(expression) => root_callable(&expression.base),
        Expr::Try(expression) => root_callable(&expression.expr),
        Expr::Paren(expression) => root_callable(&expression.expr),
        Expr::Group(expression) => root_callable(&expression.expr),
        Expr::Reference(expression) => root_callable(&expression.expr),
        _ => None,
    }
}

fn path_name(path: &SynPath) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn normalized_tokens(expression: &Expr) -> String {
    expression.to_token_stream().to_string()
}

fn contains_broad_error_check(expression: &Expr) -> bool {
    let mut visitor = BroadErrorVisitor::default();
    visitor.visit_expr(expression);
    visitor.found
}

fn is_true_literal(expression: &Expr) -> bool {
    matches!(expression, Expr::Lit(ExprLit { lit: Lit::Bool(value), .. }) if value.value())
}

fn expression_is_always_true(expression: &Expr) -> bool {
    match expression {
        Expr::Lit(ExprLit {
            lit: Lit::Bool(value),
            ..
        }) => value.value(),
        Expr::Binary(binary) => match binary.op {
            syn::BinOp::Or(_) => {
                expression_is_always_true(&binary.left) || expression_is_always_true(&binary.right)
            }
            syn::BinOp::And(_) => {
                expression_is_always_true(&binary.left) && expression_is_always_true(&binary.right)
            }
            syn::BinOp::Eq(_) => {
                is_provably_reflexive_expression(&binary.left)
                    && is_provably_reflexive_expression(&binary.right)
                    && normalized_tokens(&binary.left) == normalized_tokens(&binary.right)
            }
            _ => false,
        },
        Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Not(_)) => {
            matches!(&*unary.expr, Expr::Lit(ExprLit { lit: Lit::Bool(value), .. }) if !value.value())
        }
        Expr::Group(group) => expression_is_always_true(&group.expr),
        Expr::Paren(parenthesized) => expression_is_always_true(&parenthesized.expr),
        _ => false,
    }
}

fn is_simple_value_expression(expression: &Expr) -> bool {
    match expression {
        Expr::Path(_) | Expr::Lit(_) => true,
        Expr::Group(group) => is_simple_value_expression(&group.expr),
        Expr::Paren(parenthesized) => is_simple_value_expression(&parenthesized.expr),
        Expr::Reference(reference) => is_simple_value_expression(&reference.expr),
        _ => false,
    }
}

fn is_provably_reflexive_expression(expression: &Expr) -> bool {
    match expression {
        Expr::Lit(_) => true,
        Expr::Group(group) => is_provably_reflexive_expression(&group.expr),
        Expr::Paren(parenthesized) => is_provably_reflexive_expression(&parenthesized.expr),
        Expr::Reference(reference) => is_provably_reflexive_expression(&reference.expr),
        _ => false,
    }
}

fn is_self_comparison(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Binary(binary)
            if matches!(binary.op, syn::BinOp::Eq(_))
                && is_provably_reflexive_expression(&binary.left)
                && is_provably_reflexive_expression(&binary.right)
                && normalized_tokens(&binary.left) == normalized_tokens(&binary.right)
    )
}

fn is_possible_self_comparison(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Binary(binary)
            if matches!(binary.op, syn::BinOp::Eq(_))
                && is_simple_value_expression(&binary.left)
                && normalized_tokens(&binary.left) == normalized_tokens(&binary.right)
                && !is_provably_reflexive_expression(&binary.left)
    )
}

fn classify_assert_match(expression: &syn::ExprMatch) -> OracleKind {
    let successful_arms: Vec<_> = expression
        .arms
        .iter()
        .filter(|arm| !contains_panic(&arm.body))
        .collect();
    if successful_arms.len() == 1
        && successful_arms[0].guard.is_none()
        && is_broad_err_pattern(&successful_arms[0].pat)
        && is_empty_expression(&successful_arms[0].body)
    {
        OracleKind::BroadError
    } else {
        OracleKind::Outcome
    }
}

fn is_empty_expression(expression: &Expr) -> bool {
    matches!(expression, Expr::Tuple(tuple) if tuple.elems.is_empty())
        || matches!(expression, Expr::Block(block) if block.block.stmts.is_empty())
}

fn is_broad_err_pattern(pattern: &Pat) -> bool {
    let Pat::TupleStruct(tuple) = pattern else {
        return false;
    };
    if path_name(&tuple.path)
        .rsplit("::")
        .next()
        .is_none_or(|name| name != "Err")
        || tuple.elems.len() != 1
    {
        return false;
    }
    tuple.elems.first().is_some_and(|payload| {
        matches!(payload, Pat::Wild(_) | Pat::Ident(_))
            || matches!(payload, Pat::Reference(reference) if matches!(&*reference.pat, Pat::Ident(_)))
    })
}

fn is_broad_err_pattern_expression(expression: &Expr) -> bool {
    let Expr::Call(call) = expression else {
        return false;
    };
    if callable_name(&call.func).as_deref() != Some("Err") || call.args.len() != 1 {
        return false;
    }
    call.args.first().is_some_and(|payload| match payload {
        Expr::Infer(_) => true,
        Expr::Path(path) => local_identifier(&path.path).is_some(),
        _ => false,
    })
}

fn is_local_observer(call: &str) -> bool {
    matches!(
        call.rsplit("::").next().unwrap_or(call),
        "contains"
            | "ends_with"
            | "is_empty"
            | "is_err"
            | "is_none"
            | "is_ok"
            | "is_some"
            | "len"
            | "starts_with"
    )
}

fn looks_like_assertion_helper(name: &str) -> bool {
    matches!(name, "check" | "ensure" | "validate" | "verify")
        || [
            "assert_",
            "check_",
            "ensure_",
            "expect_",
            "validate_",
            "verify_",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn fixed_duration(arguments: &Punctuated<Expr, Token![,]>) -> bool {
    let Some(first) = arguments.first() else {
        return false;
    };
    match first {
        Expr::Lit(ExprLit {
            lit: Lit::Int(_), ..
        }) => true,
        Expr::Call(call) => {
            let name = callable_name(&call.func).unwrap_or_default();
            matches!(
                name.rsplit("::").next().unwrap_or(&name),
                "from_secs" | "from_millis" | "from_micros" | "from_nanos"
            ) && call.args.first().is_some_and(|argument| {
                matches!(
                    argument,
                    Expr::Lit(ExprLit {
                        lit: Lit::Int(_),
                        ..
                    })
                )
            })
        }
        _ => false,
    }
}

fn is_fixed_sleep_call(name: &str) -> bool {
    matches!(
        name,
        "std::thread::sleep" | "tokio::time::sleep" | "async_std::task::sleep"
    )
}

fn is_clock_call(name: &str) -> bool {
    matches!(
        name,
        "std::time::SystemTime::now"
            | "std::time::Instant::now"
            | "tokio::time::Instant::now"
            | "chrono::Utc::now"
            | "chrono::Local::now"
    )
}

fn is_random_call(name: &str) -> bool {
    matches!(
        name,
        "rand::random"
            | "rand::thread_rng"
            | "rand::rng"
            | "rand::rngs::StdRng::from_entropy"
            | "rand::rngs::SmallRng::from_entropy"
    )
}

fn is_environment_mutation(name: &str) -> bool {
    matches!(
        name,
        "std::env::set_var" | "std::env::remove_var" | "std::env::set_current_dir"
    )
}

fn may_be_hygiene_call_from_glob(name: &str) -> bool {
    let first = name.split("::").next().unwrap_or(name);
    let last = name.rsplit("::").next().unwrap_or(name);
    matches!(
        last,
        "sleep"
            | "random"
            | "thread_rng"
            | "rng"
            | "set_var"
            | "remove_var"
            | "set_current_dir"
            | "from_entropy"
    ) || (last == "now" && matches!(first, "Instant" | "SystemTime" | "Utc" | "Local"))
}

fn known_non_assertion_macro(name: &str) -> bool {
    matches!(
        name.rsplit("::").next().unwrap_or(name),
        "vec"
            | "format"
            | "println"
            | "eprintln"
            | "write"
            | "writeln"
            | "matches"
            | "panic"
            | "unreachable"
            | "todo"
    )
}

fn ignored_attribute(attributes: &[Attribute]) -> (Option<String>, Option<SourceSpan>) {
    let Some(attribute) = attributes
        .iter()
        .find(|attribute| attribute.path().is_ident("ignore"))
    else {
        return (None, None);
    };
    let reason = match &attribute.meta {
        Meta::NameValue(value) => match &value.value {
            Expr::Lit(ExprLit {
                lit: Lit::Str(reason),
                ..
            }) => Some(reason.value()),
            _ => Some(String::new()),
        },
        _ => Some(String::new()),
    };
    (reason, Some(source_span(attribute.span())))
}

fn should_panic_attribute(attributes: &[Attribute]) -> (bool, Option<String>) {
    let Some(attribute) = attributes
        .iter()
        .find(|attribute| attribute.path().is_ident("should_panic"))
    else {
        return (false, None);
    };
    let mut expected = None;
    if matches!(attribute.meta, Meta::List(_)) {
        let _ = attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("expected") {
                let value = meta.value()?;
                expected = Some(value.parse::<syn::LitStr>()?.value());
            }
            Ok(())
        });
    }
    (true, expected)
}

fn uses_paused_tokio_time(attributes: &[Attribute], aliases: &BTreeMap<String, String>) -> bool {
    attributes.iter().any(|attribute| {
        if resolve_alias(&path_name(attribute.path()), aliases) != "tokio::test" {
            return false;
        }
        let mut paused = false;
        let _ = attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("start_paused") {
                let value = meta.value()?;
                paused = value.parse::<syn::LitBool>()?.value;
            } else if meta.input.peek(Token![=]) {
                let value = meta.value()?;
                let _: Expr = value.parse()?;
            }
            Ok(())
        });
        paused
    })
}

fn source_span(span: ProcSpan) -> SourceSpan {
    let start = span.start();
    let end = span.end();
    SourceSpan {
        start: Position::new(start.line, start.column + 1),
        end: Position::new(end.line, end.column + 1),
    }
}

fn section_at(markers: &[Marker], line: usize) -> Section {
    markers
        .iter()
        .filter(|marker| marker.span.start.line < line)
        .max_by_key(|marker| marker.span.start.line)
        .map_or(Section::Body, |marker| match marker.kind {
            MarkerKind::Arrange => Section::Arrange,
            MarkerKind::Act => Section::Act,
            MarkerKind::Assert => Section::Assert,
        })
}

fn source_span_contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    !position_before(inner.start, outer.start) && !position_before(outer.end, inner.end)
}

fn position_before(left: Position, right: Position) -> bool {
    (left.line, left.column) < (right.line, right.column)
}

fn find_markers(lines: &[&str], function_span: SourceSpan) -> Vec<Marker> {
    let mut state = LexState::default();
    let mut markers = Vec::new();
    for line_number in function_span.start.line..=function_span.end.line.min(lines.len()) {
        let line = lines[line_number - 1];
        let Some((column, comment, brace_depth)) = line_comment(line, &mut state) else {
            continue;
        };
        if brace_depth != 1 {
            continue;
        }
        let trimmed = comment.trim();
        if trimmed.starts_with(['/', '!']) {
            continue;
        }
        let words: Vec<_> = trimmed
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .filter(|word| !word.is_empty())
            .collect();
        let Some(first_kind) = words.first().and_then(|word| marker_kind(word)) else {
            continue;
        };
        let kinds: Vec<_> = words.iter().filter_map(|word| marker_kind(word)).collect();
        let combined = kinds.len() > 1 || !matches!(trimmed, "Arrange" | "Act" | "Assert");
        markers.push(Marker {
            kind: first_kind,
            span: SourceSpan::line(
                line_number,
                line[..column].chars().count() + 1,
                line.chars().count() + 1,
            ),
            combined,
        });
    }
    markers
}

fn marker_kind(word: &str) -> Option<MarkerKind> {
    match word {
        "Arrange" => Some(MarkerKind::Arrange),
        "Act" => Some(MarkerKind::Act),
        "Assert" => Some(MarkerKind::Assert),
        _ => None,
    }
}

#[derive(Default)]
struct LexState {
    block_comment_depth: usize,
    raw_hashes: Option<usize>,
    string: bool,
    escaped: bool,
    brace_depth: usize,
}

fn line_comment<'a>(line: &'a str, state: &mut LexState) -> Option<(usize, &'a str, usize)> {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if let Some(hashes) = state.raw_hashes {
            if bytes[index] == b'"'
                && bytes
                    .get(index + 1..index + 1 + hashes)
                    .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
            {
                state.raw_hashes = None;
                index += hashes + 1;
            } else {
                index += 1;
            }
            continue;
        }
        if state.block_comment_depth > 0 {
            if bytes.get(index..index + 2) == Some(b"/*") {
                state.block_comment_depth += 1;
                index += 2;
            } else if bytes.get(index..index + 2) == Some(b"*/") {
                state.block_comment_depth -= 1;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if state.string {
            if state.escaped {
                state.escaped = false;
            } else if bytes[index] == b'\\' {
                state.escaped = true;
            } else if bytes[index] == b'"' {
                state.string = false;
            }
            index += 1;
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"//") {
            return Some((index, &line[index + 2..], state.brace_depth));
        }
        if bytes.get(index..index + 2) == Some(b"/*") {
            state.block_comment_depth = 1;
            index += 2;
            continue;
        }
        if let Some((hashes, consumed)) = raw_string_start(&bytes[index..]) {
            state.raw_hashes = Some(hashes);
            index += consumed;
            continue;
        }
        if bytes[index] == b'"' {
            state.string = true;
        } else if bytes[index] == b'\'' {
            if let Some(end) = char_literal_end(line, index) {
                index = end;
                continue;
            }
        } else if bytes[index] == b'{' {
            state.brace_depth += 1;
        } else if bytes[index] == b'}' {
            state.brace_depth = state.brace_depth.saturating_sub(1);
        }
        index += 1;
    }
    state.escaped = false;
    None
}

fn char_literal_end(line: &str, start: usize) -> Option<usize> {
    let suffix = line.get(start + 1..)?;
    let mut characters = suffix.char_indices();
    let (_, first) = characters.next()?;
    let content_end = if first == '\\' {
        let (escape_start, escape) = characters.next()?;
        match escape {
            'x' => {
                let mut end = escape_start + escape.len_utf8();
                for _ in 0..2 {
                    let (offset, digit) = characters.next()?;
                    if !digit.is_ascii_hexdigit() {
                        return None;
                    }
                    end = offset + digit.len_utf8();
                }
                end
            }
            'u' => {
                let (open_offset, open) = characters.next()?;
                if open != '{' {
                    return None;
                }
                let mut end = open_offset + 1;
                for (digit_index, (offset, character)) in characters.by_ref().enumerate() {
                    if character == '}' && digit_index > 0 {
                        end = offset + 1;
                        break;
                    }
                    if !character.is_ascii_hexdigit() {
                        return None;
                    }
                }
                end
            }
            _ => escape_start + escape.len_utf8(),
        }
    } else {
        first.len_utf8()
    };
    let closing = start + 1 + content_end;
    (line.as_bytes().get(closing) == Some(&b'\'')).then_some(closing + 1)
}

fn raw_string_start(bytes: &[u8]) -> Option<(usize, usize)> {
    let raw_index = if bytes.first() == Some(&b'r') {
        0
    } else if bytes.get(0..2) == Some(b"br") {
        1
    } else {
        return None;
    };
    let mut index = raw_index + 1;
    while bytes.get(index) == Some(&b'#') {
        index += 1;
    }
    (bytes.get(index) == Some(&b'"')).then_some((index - raw_index - 1, index + 1))
}

#[cfg(test)]
mod tests {
    use super::analyze_source;
    use crate::validate_tests::config::TestPolicyConfig;

    #[test]
    fn should_discover_test_with_intervening_attribute() {
        // Arrange
        let source = r#"
#[test]
#[allow(clippy::too_many_lines)]
fn should_remain_visible() {
    assert!(operation().is_ok());
}
"#;

        // Act
        let tests = analyze_source("src/lib.rs", source, &TestPolicyConfig::default())
            .expect("analyze Rust tests");

        // Assert
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "should_remain_visible");
    }

    #[test]
    fn should_discover_test_attribute_imported_under_alias() {
        // Arrange
        let source = r#"
use tokio::test as async_test;

#[async_test]
async fn should_remain_visible() {
    assert!(operation().await.is_ok());
}

mod nested {
    use super::async_test;

    #[async_test]
    async fn should_also_remain_visible() {
        assert!(operation().await.is_ok());
    }
}
"#;

        // Act
        let tests = analyze_source("src/lib.rs", source, &TestPolicyConfig::default())
            .expect("analyze Rust tests");

        // Assert
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].name, "should_remain_visible");
        assert_eq!(
            tests[1].qualified_name,
            "nested::should_also_remain_visible"
        );
    }

    #[test]
    fn should_ignore_marker_text_inside_raw_string() {
        // Arrange
        let source = r##"
#[test]
fn should_parse_text() {
    let text = r#"
// Arrange
// Act
// Assert
"#;
    assert!(!text.is_empty());
}
"##;

        // Act
        let tests = analyze_source("src/lib.rs", source, &TestPolicyConfig::default())
            .expect("analyze Rust tests");

        // Assert
        assert_eq!(tests.len(), 1);
        assert!(tests[0].markers.is_empty());
    }

    #[test]
    fn should_ignore_marker_text_inside_multiline_string() {
        // Arrange
        let source = r#"
#[test]
fn should_parse_text() {
    let text = "first line
// Arrange
// Act
// Assert
last line";
    assert!(!text.is_empty());
}
"#;

        // Act
        let tests = analyze_source("src/lib.rs", source, &TestPolicyConfig::default())
            .expect("analyze Rust tests");

        // Assert
        assert_eq!(tests.len(), 1);
        assert!(tests[0].markers.is_empty());
    }

    #[test]
    fn should_parse_markers_after_character_literal() {
        // Arrange
        let source = r#"
#[test]
fn should_parse_character() {
    // Arrange
    let quote = '\"';
    // Act
    let actual = quote.to_string();
    // Assert
    assert_eq!(actual, "\"");
}
"#;

        // Act
        let tests = analyze_source("src/lib.rs", source, &TestPolicyConfig::default())
            .expect("analyze Rust tests");

        // Assert
        assert_eq!(tests[0].markers.len(), 3);
    }

    #[test]
    fn should_ignore_markers_inside_nested_closure() {
        // Arrange
        let source = r#"
#[test]
fn should_parse_top_level_markers() {
    // Arrange
    let callback = || {
        // Act
        operation();
        // Assert
        assert!(true);
    };
    // Act
    callback();
    // Assert
    assert!(completed());
}
"#;

        // Act
        let tests = analyze_source("src/lib.rs", source, &TestPolicyConfig::default())
            .expect("analyze Rust tests");

        // Assert
        assert_eq!(tests[0].markers.len(), 3);
    }

    #[test]
    fn should_keep_function_boundary_when_string_contains_brace() {
        // Arrange
        let source = r#"
#[test]
fn should_parse_first() {
    assert_eq!("{".len(), 1);
}

#[test]
fn should_parse_second() {
    assert_eq!("}".len(), 1);
}
"#;

        // Act
        let tests = analyze_source("src/lib.rs", source, &TestPolicyConfig::default())
            .expect("analyze Rust tests");

        // Assert
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].line_count, 3);
    }

    #[test]
    fn should_report_unicode_code_point_columns() {
        // Arrange
        let source = r#"
#[test]
fn should_measure_span() {
    let café = 1; assert_eq!(café, 1);
}
"#;
        let assertion_line = source.lines().nth(3).expect("assertion source line");
        let expected_column = assertion_line
            .find("assert_eq")
            .and_then(|byte| assertion_line.get(..byte))
            .map(|prefix| prefix.chars().count())
            .expect("assertion source column")
            + 1;

        // Act
        let tests = analyze_source("src/lib.rs", source, &TestPolicyConfig::default())
            .expect("analyze Rust tests");

        // Assert
        assert_eq!(tests[0].oracles[0].span.start.column, expected_column);
    }

    #[test]
    fn should_not_treat_actual_as_an_act_marker() {
        // Arrange
        let source = r#"
#[test]
fn should_parse_comment() {
    // Actual response is intentionally named below.
    let actual = operation();
    assert_eq!(actual, 1);
}
"#;

        // Act
        let tests = analyze_source("src/lib.rs", source, &TestPolicyConfig::default())
            .expect("analyze Rust tests");

        // Assert
        assert!(tests[0].markers.is_empty());
    }
}
