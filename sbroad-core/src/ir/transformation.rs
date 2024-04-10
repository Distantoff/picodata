//! Plan tree transformation module.
//!
//! Contains rule-based transformations.

pub mod bool_in;
pub mod dnf;
pub mod equality_propagation;
pub mod merge_tuples;
pub mod not_push_down;
pub mod redistribution;
pub mod split_columns;

use super::tree::traversal::{PostOrder, PostOrderWithFilter, EXPR_CAPACITY};
use crate::errors::{Entity, SbroadError};
use crate::ir::expression::Expression;
use crate::ir::operator::{Bool, Relational};
use crate::ir::{Node, Plan};
use std::collections::HashMap;

pub type ExprId = usize;
/// Helper type representing map of (`old_expr_id` -> `changed_expr_id`).
pub type OldNewExpressionMap = HashMap<ExprId, ExprId>;

/// Pair of (old tree id, transformed tree id).
pub type OldNewTopIdPair = (ExprId, ExprId);

/// Function passed for being applied on WHERE|ON condition roots.
/// It returns pair of (`old_tree_id`, `new_tree_id`).
pub type TransformFunction<'func> =
    &'func dyn Fn(&mut Plan, ExprId) -> Result<OldNewTopIdPair, SbroadError>;

impl Plan {
    /// Concatenates trivalents (boolean or NULL expressions) to the AND node.
    ///
    /// # Errors
    /// - If the left or right child is not a trivalent.
    pub fn concat_and(
        &mut self,
        left_expr_id: usize,
        right_expr_id: usize,
    ) -> Result<usize, SbroadError> {
        if !self.is_trivalent(left_expr_id)? {
            return Err(SbroadError::Invalid(
                Entity::Expression,
                Some(
                    format!(
                        "Left expression is not a boolean expression or NULL: {:?}",
                        self.get_expression_node(left_expr_id)?
                    )
                    .into(),
                ),
            ));
        }
        if !self.is_trivalent(right_expr_id)? {
            return Err(SbroadError::Invalid(
                Entity::Expression,
                Some(
                    format!(
                        "Right expression is not a boolean expression or NULL: {:?}",
                        self.get_expression_node(right_expr_id)?
                    )
                    .into(),
                ),
            ));
        }
        self.add_cond(left_expr_id, Bool::And, right_expr_id)
    }

    /// Concatenates trivalents (boolean or NULL expressions) to the OR node.
    ///
    /// # Errors
    /// - If the left or right child is not a trivalent.
    pub fn concat_or(
        &mut self,
        left_expr_id: usize,
        right_expr_id: usize,
    ) -> Result<usize, SbroadError> {
        if !self.is_trivalent(left_expr_id)? {
            return Err(SbroadError::Invalid(
                Entity::Expression,
                Some(
                    format!(
                        "left expression is not a boolean expression or NULL: {:?}",
                        self.get_expression_node(left_expr_id)?
                    )
                    .into(),
                ),
            ));
        }
        if !self.is_trivalent(right_expr_id)? {
            return Err(SbroadError::Invalid(
                Entity::Expression,
                Some(
                    format!(
                        "right expression is not a boolean expression or NULL: {:?}",
                        self.get_expression_node(right_expr_id)?
                    )
                    .into(),
                ),
            ));
        }
        self.add_cond(left_expr_id, Bool::Or, right_expr_id)
    }

    /// Transforms all expression subtrees in the plan
    /// for selection and join nodes using the
    /// defined function.
    ///
    /// # Errors
    /// - If failed to get the plan top node.
    /// - If relational iterator returns non-relational node.
    /// - If failed to transform the expression subtree.
    pub fn transform_expr_trees(&mut self, f: TransformFunction) -> Result<(), SbroadError> {
        let top_id = self.get_top()?;
        let mut ir_tree = PostOrder::with_capacity(|node| self.nodes.rel_iter(node), EXPR_CAPACITY);
        ir_tree.populate_nodes(top_id);
        let nodes = ir_tree.take_nodes();
        for (_, id) in &nodes {
            let rel = self.get_relation_node(*id)?;
            let (old_tree_id, new_tree_id) = match rel {
                Relational::Selection {
                    filter: tree_id, ..
                }
                | Relational::Join {
                    condition: tree_id, ..
                } => f(self, *tree_id)?,
                _ => continue,
            };
            if old_tree_id != new_tree_id {
                self.undo.add(new_tree_id, old_tree_id);
            }
            let rel = self.get_mut_relation_node(*id)?;
            match rel {
                Relational::Selection {
                    filter: tree_id, ..
                }
                | Relational::Join {
                    condition: tree_id, ..
                } => {
                    *tree_id = new_tree_id;
                }
                _ => continue,
            }
        }
        Ok(())
    }

    /// Replaces boolean operators in an expression subtree with the
    /// new boolean expressions produced by the user defined function.
    ///
    /// # Errors
    ///  If expression subtree iterator returns a non-expression node.
    pub fn expr_tree_replace_bool(
        &mut self,
        top_id: usize,
        f: TransformFunction,
        ops: &[Bool],
    ) -> Result<OldNewTopIdPair, SbroadError> {
        let mut map: OldNewExpressionMap = HashMap::new();
        // Note, that filter accepts nodes:
        // * On which we'd like to apply transformation
        // * That will contain transformed nodes as children
        let filter = |node_id: usize| -> bool {
            if let Ok(Node::Expression(
                Expression::Bool { .. }
                | Expression::ExprInParentheses { .. }
                | Expression::Arithmetic { .. }
                | Expression::Alias { .. }
                | Expression::Row { .. }
                | Expression::Cast { .. }
                | Expression::StableFunction { .. }
                | Expression::Unary { .. },
            )) = self.get_node(node_id)
            {
                return true;
            }
            false
        };
        let mut subtree = PostOrderWithFilter::with_capacity(
            |node| self.nodes.expr_iter(node, false),
            EXPR_CAPACITY,
            Box::new(filter),
        );
        subtree.populate_nodes(top_id);
        let nodes = subtree.take_nodes();
        drop(subtree);
        for (_, id) in &nodes {
            let expr = self.get_expression_node(*id)?;
            if let Expression::Bool { op, .. } = expr {
                if ops.contains(op) || ops.is_empty() {
                    let (old_top_id, new_top_id) = f(self, *id)?;
                    if old_top_id != new_top_id {
                        map.insert(*id, new_top_id);
                    }
                }
            }
        }
        let (old_top_id, new_top_id) = if map.is_empty() {
            (top_id, top_id)
        } else {
            let old_top_id = if map.get(&top_id).is_some() && map.len() == 1 {
                top_id
            } else {
                self.clone_expr_subtree(top_id)?
            };
            let mut new_top_id = top_id;
            for (_, id) in &nodes {
                let expr = self.get_mut_expression_node(*id)?;
                // For all expressions in the subtree tries to replace their children
                // with the new nodes from the map.
                //
                // XXX: If you add a new expression type to the match, make sure to
                // add it to the filter above.
                match expr {
                    Expression::Alias { child, .. }
                    | Expression::ExprInParentheses { child, .. }
                    | Expression::Cast { child, .. }
                    | Expression::Unary { child, .. } => {
                        if let Some(new_id) = map.get(child) {
                            *child = *new_id;
                        }
                    }
                    Expression::Bool { left, right, .. }
                    | Expression::Arithmetic { left, right, .. } => {
                        if let Some(new_id) = map.get(left) {
                            *left = *new_id;
                        }
                        if let Some(new_id) = map.get(right) {
                            *right = *new_id;
                        }
                    }
                    Expression::Row { list, .. }
                    | Expression::StableFunction { children: list, .. } => {
                        for id in list {
                            if let Some(new_id) = map.get(id) {
                                *id = *new_id;
                            }
                        }
                    }
                    Expression::Concat { .. }
                    | Expression::Constant { .. }
                    | Expression::Reference { .. }
                    | Expression::CountAsterisk => {}
                }
            }
            // Checks if the top node is a new node.
            if let Some(new_id) = map.get(&top_id) {
                new_top_id = *new_id;
            }
            (old_top_id, new_top_id)
        };
        Ok((old_top_id, new_top_id))
    }
}

#[cfg(test)]
pub mod helpers;
