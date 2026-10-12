use std::collections::BTreeSet;

use pine_ir::{CallSiteId, HirCallArg, HirExpr, HirExprKind, HirProgram, HirStmtKind};

use super::{CallFamily, CallPlan};
use crate::builtins::ta::TaOpcode;

fn program(text: &str) -> HirProgram {
    let source = pine_syntax::SourceFile::new("ta_state_plan.pine", text);
    let analysis = pine_sema::analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn plotted_value(program: &mut HirProgram, index: usize) -> &mut HirExpr {
    program
        .statements
        .iter_mut()
        .filter_map(|statement| {
            let HirStmtKind::Expr(expr) = &mut statement.kind else {
                return None;
            };
            let HirExprKind::Call { callee, args, .. } = &mut expr.kind else {
                return None;
            };
            (callee == "plot").then(|| &mut args[0].value)
        })
        .nth(index)
        .unwrap()
}

fn plotted_call(program: &HirProgram, index: usize) -> (&str, CallSiteId, &[HirCallArg]) {
    program
        .statements
        .iter()
        .filter_map(|statement| {
            let HirStmtKind::Expr(expr) = &statement.kind else {
                return None;
            };
            let HirExprKind::Call { callee, args, .. } = &expr.kind else {
                return None;
            };
            if callee != "plot" {
                return None;
            }
            let HirExprKind::Call {
                callee,
                call_site_id,
                args,
            } = &args[0].value.kind
            else {
                panic!("expected plotted call");
            };
            Some((callee.as_str(), *call_site_id, args.as_slice()))
        })
        .nth(index)
        .unwrap()
}

fn set_site(program: &mut HirProgram, index: usize, site: u32) {
    let HirExprKind::Call { call_site_id, .. } = &mut plotted_value(program, index).kind else {
        panic!("expected call");
    };
    *call_site_id = CallSiteId(site);
}

fn actual_sites(program: &HirProgram) -> BTreeSet<u32> {
    let mut sites = BTreeSet::new();
    super::super::hir_walk::statements(&program.statements, &mut |expr| {
        if let HirExprKind::Call { call_site_id, .. } = &expr.kind {
            sites.insert(call_site_id.0);
        }
    });
    sites
}

fn assert_aliases_are_unused_and_distinct(plan: &CallPlan, program: &HirProgram) {
    let actual = actual_sites(program);
    let aliases: BTreeSet<_> = plan.ta_state_sites.values().map(|site| site.0).collect();
    assert_eq!(aliases.len(), plan.ta_state_sites.len());
    assert!(actual.is_disjoint(&aliases));
    for index in 0..program.statements.len() - 1 {
        let (callee, site, args) = plotted_call(program, index);
        let dispatch = plan.dispatch(site, callee, args);
        if let Some(opcode) = dispatch.ta_opcode {
            assert_eq!(
                dispatch.state_site,
                plan.ta_state_sites
                    .get(&(site, opcode))
                    .copied()
                    .unwrap_or(site)
            );
        } else {
            assert_eq!(dispatch.state_site, site);
        }
    }
}

#[test]
fn normal_unique_ids_keep_cached_state_sites_without_alias_storage() {
    let mut program = program(
        "//@version=6\nindicator(\"sites\")\nplot(ta.sma(close, 2))\nplot(ta.wma(close, 2))\nplot(fixnan(close))\n",
    );
    for next in [0, 1, u32::MAX] {
        program.next_call_site_id = next;
        let before = format!("{program:?}");
        let plan = CallPlan::from_program(&program);
        assert!(plan.ta_state_sites.is_empty());
        super::super::hir_walk::statements(&program.statements, &mut |expr| {
            if let HirExprKind::Call {
                callee,
                call_site_id,
                args,
            } = &expr.kind
            {
                let binding = plan.binding(*call_site_id).unwrap();
                assert_eq!(binding.callee, *callee);
                assert_eq!(binding.dispatch.state_site, *call_site_id);
                assert_eq!(plan.dispatch(*call_site_id, callee, args), binding.dispatch);
            }
        });
        assert_eq!(format!("{program:?}"), before);
    }
}

#[test]
fn repeated_same_callee_keeps_original_sparse_id_and_named_argument_fallback() {
    let mut program = program(
        "//@version=6\nindicator(\"sites\")\nplot(ta.sma(close, 2))\nplot(ta.sma(close, 2))\n",
    );
    for index in 0..2 {
        set_site(&mut program, index, u32::MAX);
    }
    let HirExprKind::Call { args, .. } = &mut plotted_value(&mut program, 1).kind else {
        panic!("expected SMA");
    };
    args[0].name = Some("source".to_owned());
    args[1].name = Some("length".to_owned());
    args.reverse();
    let plan = CallPlan::from_program(&program);
    assert!(plan.ta_state_sites.is_empty());
    let binding = plan.binding(CallSiteId(u32::MAX)).unwrap();
    assert!(!binding.dispatch.positional_args);
    for index in 0..2 {
        let (callee, site, args) = plotted_call(&program, index);
        let dispatch = plan.dispatch(site, callee, args);
        assert_eq!(dispatch.state_site, CallSiteId(u32::MAX));
        assert!(!dispatch.positional_args);
    }
}

#[test]
fn dense_holes_and_sparse_max_conflicts_get_deterministic_nonoverlapping_aliases() {
    let mut program = program(
        "//@version=6\nindicator(\"sites\")\nplot(ta.sma(close, 2))\nplot(ta.wma(close, 2))\nplot(ta.ema(close, 2))\nplot(ta.sma(close, 2))\nplot(math.abs(close))\n",
    );
    for (index, statement) in program.statements.iter_mut().enumerate() {
        let HirStmtKind::Expr(expr) = &mut statement.kind else {
            panic!("expected expression");
        };
        let HirExprKind::Call { call_site_id, .. } = &mut expr.kind else {
            panic!("expected outer call");
        };
        *call_site_id = CallSiteId(index as u32 * 2);
    }
    for (index, site) in [u32::MAX, u32::MAX, 4, 4, 12].into_iter().enumerate() {
        set_site(&mut program, index, site);
    }
    assert_eq!(
        actual_sites(&program),
        BTreeSet::from([0, 2, 4, 6, 8, 10, 12, u32::MAX])
    );
    let expected = [
        ((CallSiteId(4), TaOpcode::Sma), CallSiteId(1)),
        ((CallSiteId(4), TaOpcode::Ema), CallSiteId(3)),
        ((CallSiteId(u32::MAX), TaOpcode::Sma), CallSiteId(5)),
        ((CallSiteId(u32::MAX), TaOpcode::Wma), CallSiteId(7)),
    ];
    for next in [0, 13, u32::MAX] {
        program.next_call_site_id = next;
        let plan = CallPlan::from_program(&program);
        assert_eq!(plan.ta_state_sites.len(), expected.len());
        for (key, alias) in expected {
            assert_eq!(plan.ta_state_sites.get(&key), Some(&alias));
        }
        assert!(plan.binding(CallSiteId(4)).is_none());
        assert!(plan.binding(CallSiteId(u32::MAX)).is_none());
        assert!(plan.dense.len() <= actual_sites(&program).len() * 2);
        assert_aliases_are_unused_and_distinct(&plan, &program);
    }
    let plan = CallPlan::from_program(&program);
    program.statements[1..].reverse();
    let reversed = CallPlan::from_program(&program);
    assert_eq!(reversed.ta_state_sites, plan.ta_state_sites);
}

#[test]
fn mixed_ta_and_fixnan_collision_aliases_each_ta_and_preserves_external_dispatch() {
    let mut program = program(
        "//@version=6\nindicator(\"sites\")\nplot(ta.cum(close))\nplot(fixnan(close))\nplot(ta.sma(close, 2))\n",
    );
    for index in 0..3 {
        set_site(&mut program, index, u32::MAX);
    }
    let HirExprKind::Call { args, .. } = &mut plotted_value(&mut program, 2).kind else {
        panic!("expected SMA");
    };
    args[0].name = Some("source".to_owned());
    args[1].name = Some("length".to_owned());
    args.reverse();
    let plan = CallPlan::from_program(&program);
    assert!(plan.binding(CallSiteId(u32::MAX)).is_none());
    assert_eq!(plan.ta_state_sites.len(), 2);
    let (callee, site, args) = plotted_call(&program, 1);
    let fixnan = plan.dispatch(site, callee, args);
    assert_eq!(fixnan.family, CallFamily::Variable);
    assert_eq!(fixnan.state_site, site);
    let (callee, site, args) = plotted_call(&program, 2);
    assert!(!plan.dispatch(site, callee, args).positional_args);
    assert_aliases_are_unused_and_distinct(&plan, &program);
    // Guard mismatches and unrecorded calls cannot acquire an alias for an
    // operation that was never declared at that conflicting ID.
    assert_eq!(
        plan.dispatch(CallSiteId(u32::MAX), "ta.ema", &[])
            .state_site,
        CallSiteId(u32::MAX)
    );
    assert_eq!(
        plan.dispatch(CallSiteId(u32::MAX - 1), "ta.cum", &[])
            .state_site,
        CallSiteId(u32::MAX - 1)
    );
    assert_eq!(
        plan.dispatch(CallSiteId(u32::MAX), "ta.unknown", &[])
            .state_site,
        CallSiteId(u32::MAX)
    );
}

#[test]
fn alias_count_depends_on_distinct_conflicting_opcodes_not_call_occurrences() {
    let mut source = "//@version=6\nindicator(\"sites\")\n".to_owned();
    for index in 0..96 {
        let callee = if index % 3 == 0 { "ta.wma" } else { "ta.sma" };
        source.push_str(&format!("plot({callee}(close, 2))\n"));
    }
    source.push_str("plot(ta.ema(close, 2))\n");
    let mut program = program(&source);
    for index in 0..96 {
        set_site(&mut program, index, u32::MAX);
    }
    let plan = CallPlan::from_program(&program);
    assert_eq!(plan.ta_state_sites.len(), 2);
    assert_aliases_are_unused_and_distinct(&plan, &program);
    let (callee, site, args) = plotted_call(&program, 96);
    assert_eq!(plan.dispatch(site, callee, args).state_site, site);
    for index in 0..96 {
        let (callee, site, args) = plotted_call(&program, index);
        let first = if callee == "ta.wma" { 0 } else { 1 };
        let (first_callee, first_site, first_args) = plotted_call(&program, first);
        assert_eq!(
            plan.dispatch(site, callee, args).state_site,
            plan.dispatch(first_site, first_callee, first_args)
                .state_site
        );
    }
}

#[test]
fn conflicts_without_recognized_ta_calls_create_no_aliases() {
    let mut program =
        program("//@version=6\nindicator(\"sites\")\nplot(fixnan(close))\nplot(math.abs(close))\n");
    set_site(&mut program, 0, u32::MAX);
    set_site(&mut program, 1, u32::MAX);
    let plan = CallPlan::from_program(&program);
    assert!(plan.binding(CallSiteId(u32::MAX)).is_none());
    assert!(plan.ta_state_sites.is_empty());
    assert_aliases_are_unused_and_distinct(&plan, &program);
}
