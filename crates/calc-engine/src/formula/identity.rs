//! Formula equality across snapshots whose row and column positions differ.
use super::ast::{Coord, Expr};
use crate::{Workbook, model::Axis};
use serde_json::{Value, json};

fn coordinate(axis: Option<&Axis>, coord: Coord, origin: u32) -> Value {
    let target = match coord.resolve(origin) {
        Some(pos) => match axis.and_then(|a| a.id_at(pos)) {
            Some(id) => json!(["id", id]),
            None => json!(["position", pos]),
        },
        None => json!(["invalid", coord]),
    };
    json!([coord.is_abs(), target])
}

/// A comparison key based on referenced stable cell identities, retaining absolute/relative intent.
/// `row` and `col` are the current positions of the formula cell.
pub fn stable_key(wb: &Workbook, tab: u32, row: u32, col: u32, expr: &Expr) -> String {
    fn rewrite(value: &mut Value, wb: &Workbook, tab: u32, row: u32, col: u32) {
        match value {
            Value::Object(object) => {
                if object.len() == 1 {
                    for kind in ["Cell", "Spill", "Range"] {
                        if let Some(reference) = object.get(kind) {
                            // This is a serialized Expr leaf, not arbitrary input JSON.
                            let explicit = reference
                                .get("tab")
                                .and_then(Value::as_u64)
                                .map(|id| id as u32);
                            let target = explicit.unwrap_or(tab);
                            let sheet = wb.tab(target);
                            let rows = sheet.map(|t| &t.rows);
                            let cols = sheet.map(|t| &t.cols);
                            let normalized = if kind == "Range" {
                                let axis = |field: &str, ids: Option<&Axis>, origin| {
                                    let pair: Option<(Coord, Coord)> =
                                        serde_json::from_value(reference[field].clone())
                                            .expect("serialized reference");
                                    pair.map(|(a, b)| {
                                        json!([
                                            coordinate(ids, a, origin),
                                            coordinate(ids, b, origin)
                                        ])
                                    })
                                };
                                json!([
                                    kind,
                                    explicit.is_some(),
                                    target,
                                    axis("rows", rows, row),
                                    axis("cols", cols, col)
                                ])
                            } else {
                                let r: Coord = serde_json::from_value(reference["row"].clone())
                                    .expect("serialized reference");
                                let c: Coord = serde_json::from_value(reference["col"].clone())
                                    .expect("serialized reference");
                                json!([
                                    kind,
                                    explicit.is_some(),
                                    target,
                                    coordinate(rows, r, row),
                                    coordinate(cols, c, col)
                                ])
                            };
                            // The marker cannot occur in the serialized Expr enum, preventing key collisions.
                            *value = json!({"StableReference":normalized});
                            return;
                        }
                    }
                }
                for child in object.values_mut() {
                    rewrite(child, wb, tab, row, col);
                }
            }
            Value::Array(array) => {
                for child in array {
                    rewrite(child, wb, tab, row, col);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::to_value(expr).expect("formula expression");
    rewrite(&mut value, wb, tab, row, col);
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        formula::parse::parse_formula,
        ops::{Actor, CellWrite, Op},
    };
    #[test]
    fn stable_reference_keys_follow_identity_but_keep_absolute_intent() {
        let mut wb = Workbook::new();
        let tab = wb.tabs[0].id;
        let actor = Actor {
            account: "test",
            at: 0,
        };
        let op = wb
            .prepare_writes(
                tab,
                vec![(
                    1,
                    1,
                    CellWrite {
                        input: Some("20".into()),
                        ..Default::default()
                    },
                )],
            )
            .unwrap();
        wb.apply(&op, &actor).unwrap();
        let before = parse_formula("$B$2+B2+SUM(B1:B2)", 1, 0, &wb.names()).unwrap();
        let key = stable_key(&wb, tab, 1, 0, &before);
        let op = wb.prepare_insert(tab, true, 0, 1).unwrap();
        wb.apply(&op, &actor).unwrap();
        let after = parse_formula("$B$3+B3+SUM(B2:B3)", 2, 0, &wb.names()).unwrap();
        assert_eq!(key, stable_key(&wb, tab, 2, 0, &after));
        let changed = parse_formula("B3+B3+SUM(B2:B3)", 2, 0, &wb.names()).unwrap();
        assert_ne!(key, stable_key(&wb, tab, 2, 0, &changed));
        let left = parse_formula("IF(,B3)", 2, 0, &wb.names()).unwrap();
        let right = parse_formula("IF(B3,)", 2, 0, &wb.names()).unwrap();
        assert_ne!(
            stable_key(&wb, tab, 2, 0, &left),
            stable_key(&wb, tab, 2, 0, &right)
        );
        wb.apply(
            &Op::DeleteRows {
                tab,
                ids: vec![wb.tab(tab).unwrap().rows.id_at(0).unwrap()],
            },
            &actor,
        )
        .unwrap();
        assert_eq!(key, stable_key(&wb, tab, 1, 0, &before));
    }
}
