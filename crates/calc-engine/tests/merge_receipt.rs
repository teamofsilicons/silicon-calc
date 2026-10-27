use calc_engine::{
    Value, Workbook,
    addr::Rect,
    diff::MergeChoice,
    merge_ids::accept_merge,
    ops::{Actor, CellWrite, Op},
};
fn write(w: &mut Workbook, t: u32, row: u32, col: u32, input: &str) {
    let op = w
        .prepare_writes(
            t,
            vec![(
                row,
                col,
                CellWrite {
                    input: Some(input.into()),
                    clear: true,
                    ..Default::default()
                },
            )],
        )
        .unwrap();
    w.apply(
        &op,
        &Actor {
            account: "c:receipt",
            at: 1_800_000_000,
        },
    )
    .unwrap();
}

fn delete_first_row(wb: &mut Workbook, tab: u32) {
    let id = wb.tab(tab).unwrap().rows.id_at(0).unwrap();
    wb.apply(
        &Op::DeleteRows { tab, ids: vec![id] },
        &Actor {
            account: "c:receipt",
            at: 1_800_000_000,
        },
    )
    .unwrap();
}

#[test]
fn partial_receipt_maps_absolute_and_relative_references_after_deleted_prefix() {
    let mut base = Workbook::new();
    let tab = base.tabs[0].id;
    for (r, v) in ["10", "20", "30"].into_iter().enumerate() {
        write(&mut base, tab, r as u32, 1, v);
    }
    let mut source = base.clone();
    delete_first_row(&mut source, tab);
    write(&mut source, tab, 0, 0, "=SUM($B$1:B2)+$B$1");
    let mut accepted = accept_merge(
        &base,
        &source,
        &MergeChoice {
            area: Some((tab, Rect::new(0, 0, 0, 0))),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        accepted.tab(tab).unwrap().rows.ids(),
        base.tab(tab).unwrap().rows.ids()
    );
    assert_eq!(
        accepted.formula_text(tab, 1, 0).as_deref(),
        Some("SUM($B$2:B3)+$B$2")
    );
    assert_eq!(
        accepted.tab(tab).unwrap().value_at(1, 0),
        Value::Number(70.0)
    );
    write(&mut accepted, tab, 1, 1, "25");
    assert_eq!(
        accepted.tab(tab).unwrap().value_at(1, 0),
        Value::Number(80.0)
    );
    assert_eq!(
        accepted.tab(tab).unwrap().value_at(0, 1),
        Value::Number(10.0)
    );
}

#[test]
fn selected_tab_receipt_maps_forward_references_after_all_layouts_exist() {
    let mut base = Workbook::new();
    let first = base.tabs[0].id;
    let later = base.add_tab("Later", None);
    write(&mut base, later, 0, 0, "10");
    write(&mut base, later, 1, 0, "20");
    let mut source = base.clone();
    delete_first_row(&mut source, later);
    write(&mut source, first, 0, 0, "=Later!$A$1");
    let mut accepted = accept_merge(
        &base,
        &source,
        &MergeChoice {
            tabs: vec![first, later],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        accepted.formula_text(first, 0, 0).as_deref(),
        Some("Later!$A$1")
    );
    write(&mut accepted, later, 0, 0, "25");
    assert_eq!(
        accepted.tab(first).unwrap().value_at(0, 0),
        Value::Number(25.0)
    );
}

#[test]
fn positional_remapping_preserves_mixed_refs_spills_and_whole_axes() {
    use calc_engine::{
        formula::parse::{parse_formula, to_a1},
        merge_ids::{MergeIds, remap, remap_expr_positions},
    };
    use std::collections::BTreeMap;
    let mut source = Workbook::new();
    let tab = source.tabs[0].id;
    write(&mut source, tab, 2, 2, "7");
    let ids = MergeIds {
        tabs: BTreeMap::from([(tab, 10)]),
        rows: BTreeMap::from([(tab, BTreeMap::from([(0, 5), (1, 6), (2, 7)]))]),
        cols: BTreeMap::from([(tab, BTreeMap::from([(0, 8), (1, 9), (2, 10)]))]),
    };
    let mut dest = remap(&source, &ids).unwrap();
    let op = dest.prepare_insert(10, true, 0, 1).unwrap();
    dest.apply(
        &op,
        &Actor {
            account: "c:receipt",
            at: 1_800_000_000,
        },
    )
    .unwrap();
    let op = dest.prepare_insert(10, false, 0, 1).unwrap();
    dest.apply(
        &op,
        &Actor {
            account: "c:receipt",
            at: 1_800_000_000,
        },
    )
    .unwrap();
    for (text, expected) in [
        ("$A1+B$2+$C$3", "$B2+C$3+$D$4"),
        ("A1#", "B2#"),
        ("SUM(A:C)+SUM(1:3)", "SUM(B:D)+SUM(2:4)"),
    ] {
        let expr = parse_formula(text, 0, 0, &source.names()).unwrap();
        let mapped = remap_expr_positions(&source, &dest, tab, 10, (0, 0), (1, 1), &expr, &ids);
        assert_eq!(to_a1(&mapped, 1, 1, &dest.names()), expected);
    }
}
#[test]
fn partial_receipt_keeps_pending_cells_and_formula_identity_for_unselected_tabs() {
    let mut base = Workbook::new();
    let t = base.tabs[0].id;
    write(&mut base, t, 0, 0, "10");
    write(&mut base, t, 0, 1, "20");
    let mut source = base.clone();
    let other = source.add_tab("Future", None);
    write(&mut source, other, 0, 0, "7");
    write(&mut source, t, 0, 0, "=Future!A1*2");
    write(&mut source, t, 0, 1, "30");
    let choice = MergeChoice {
        area: Some((t, Rect::new(0, 0, 0, 0))),
        ..Default::default()
    };
    let accepted = accept_merge(&base, &source, &choice).unwrap();
    assert!(accepted.tab(other).is_none());
    assert_eq!(accepted.tab(t).unwrap().value_at(0, 1), Value::Number(20.0));
    assert_eq!(accepted.tab(t).unwrap().value_at(0, 0), Value::Number(10.0));
    assert!(accepted.formula_text(t, 0, 0).is_none());
    let whole_tab = accept_merge(
        &base,
        &source,
        &MergeChoice {
            tabs: vec![t],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        whole_tab.tab(t).unwrap().value_at(0, 0),
        Value::Number(10.0)
    );
    assert!(whole_tab.formula_text(t, 0, 0).is_none());
    let next = accept_merge(
        &accepted,
        &source,
        &MergeChoice {
            tabs: vec![other],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(next.tab(t).unwrap().value_at(0, 0), Value::Number(10.0));
    assert!(next.formula_text(t, 0, 0).is_none());
    assert_eq!(next.tab(t).unwrap().value_at(0, 1), Value::Number(20.0));
    let pending = calc_engine::diff::diff(&next, &source, 100, None);
    assert!(
        pending
            .cells
            .iter()
            .any(|c| c.tab == t && c.address == "A1")
    );
    let accepted_formula = accept_merge(&next, &source, &choice).unwrap();
    assert_eq!(
        accepted_formula.formula_text(t, 0, 0).as_deref(),
        Some("Future!A1*2")
    );
}

#[test]
fn partial_receipt_keeps_formula_pending_until_nonblank_axis_dependency_arrives() {
    let mut base = Workbook::new();
    let tab = base.tabs[0].id;
    write(&mut base, tab, 0, 0, "10");
    let mut source = base.clone();
    write(&mut source, tab, 2, 1, "7");
    write(&mut source, tab, 0, 0, "=$B$3*2");
    let selected = MergeChoice {
        area: Some((tab, Rect::new(0, 0, 0, 0))),
        ..Default::default()
    };
    let accepted = accept_merge(&base, &source, &selected).unwrap();
    assert_eq!(
        accepted.tab(tab).unwrap().value_at(0, 0),
        Value::Number(10.0)
    );
    assert!(accepted.formula_text(tab, 0, 0).is_none());
    let with_dependency = accept_merge(
        &accepted,
        &source,
        &MergeChoice {
            area: Some((tab, Rect::new(2, 1, 2, 1))),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        with_dependency.tab(tab).unwrap().value_at(0, 0),
        Value::Number(10.0)
    );
    let finished = accept_merge(&with_dependency, &source, &selected).unwrap();
    assert_eq!(finished.formula_text(tab, 0, 0).as_deref(), Some("$B$3*2"));
    assert_eq!(
        finished.tab(tab).unwrap().value_at(0, 0),
        Value::Number(14.0)
    );
}
