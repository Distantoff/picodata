use std::cell::RefCell;

use super::TreeIterator;
use crate::ir::expression::Expression;
use crate::ir::operator::Bool;
use crate::ir::{Node, Nodes};

trait AndTreeIterator<'nodes>: TreeIterator<'nodes> {}

/// Children iterator for "and"-ed expression chains.
///
/// The iterator returns the next child for the chained `Bool::And` nodes.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug)]
pub struct AndIterator<'n> {
    current: usize,
    child: RefCell<usize>,
    nodes: &'n Nodes,
}

impl<'nodes> TreeIterator<'nodes> for AndIterator<'nodes> {
    fn get_current(&self) -> usize {
        self.current
    }

    fn get_child(&self) -> &RefCell<usize> {
        &self.child
    }

    fn get_nodes(&self) -> &'nodes Nodes {
        self.nodes
    }
}

impl<'nodes> AndTreeIterator<'nodes> for AndIterator<'nodes> {}

impl<'n> Nodes {
    #[must_use]
    pub fn and_iter(&'n self, current: usize) -> AndIterator<'n> {
        AndIterator {
            current,
            child: RefCell::new(0),
            nodes: self,
        }
    }
}

impl<'n> Iterator for AndIterator<'n> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        and_next(self).copied()
    }
}

fn and_next<'nodes>(iter: &mut impl AndTreeIterator<'nodes>) -> Option<&'nodes usize> {
    let node = iter.get_nodes().arena.get(iter.get_current());
    if let Some(Node::Expression(Expression::Bool {
        left, op, right, ..
    })) = node
    {
        if *op != Bool::And {
            return None;
        }
        let child_step = *iter.get_child().borrow();
        if child_step == 0 {
            *iter.get_child().borrow_mut() += 1;
            return Some(left);
        } else if child_step == 1 {
            *iter.get_child().borrow_mut() += 1;
            return Some(right);
        }
        None
    } else {
        None
    }
}
