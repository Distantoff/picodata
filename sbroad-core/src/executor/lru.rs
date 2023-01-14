use crate::errors::{Action, Entity, SbroadError};
use std::collections::{hash_map::Entry, HashMap};
use std::fmt::Debug;

pub const DEFAULT_CAPACITY: usize = 50;

pub type EvictFn<Value> = Box<dyn Fn(&mut Value) -> Result<(), SbroadError>>;

pub trait Cache<Key, Value> {
    /// Builds a new cache with the given capacity.
    ///
    /// # Errors
    /// - Capacity is not valid (zero).
    fn new(capacity: usize, evict_fn: Option<EvictFn<Value>>) -> Result<Self, SbroadError>
    where
        Self: Sized;

    /// Returns a value from the cache.
    ///
    /// # Errors
    /// - Internal error (should never happen).
    fn get(&mut self, key: &Key) -> Result<Option<&Value>, SbroadError>;

    /// Inserts a key-value pair into the cache.
    ///
    /// # Errors
    /// - Internal error (should never happen).
    fn put(&mut self, key: Key, value: Value) -> Result<(), SbroadError>;
}

#[derive(Debug)]
struct LRUNode<Key, Value>
where
    Key: Clone,
    Value: Default + Debug,
{
    /// The value of the node.
    value: Value,
    /// Next node key in a hash map.
    next: Option<Key>,
    /// Previous node key in a hash map.
    prev: Option<Key>,
}

impl<Key, Value> LRUNode<Key, Value>
where
    Key: Clone,
    Value: Default + Debug,
{
    fn new(value: Value) -> Self {
        LRUNode {
            value,
            next: None,
            prev: None,
        }
    }

    fn sentinel() -> Self {
        LRUNode::new(Value::default())
    }

    fn replace_next(&mut self, next: Option<Key>) {
        self.next = next;
    }

    fn replace_prev(&mut self, prev: Option<Key>) {
        self.prev = prev;
    }
}

pub struct LRUCache<Key, Value>
where
    Key: Clone,
    Value: Default + Debug,
{
    /// The capacity of the cache.
    capacity: usize,
    /// Actual amount of nodes in the cache.
    size: usize,
    /// `None` key is reserved for the LRU sentinel head.
    map: HashMap<Option<Key>, LRUNode<Key, Value>>,
    // A function applied to the value before evicting it from the cache.
    evict_fn: Option<EvictFn<Value>>,
}

impl<Key, Value> LRUCache<Key, Value>
where
    Value: Default + Debug,
    Key: Clone + Eq + std::hash::Hash + std::fmt::Debug,
{
    fn get_node_or_none(&self, key: &Option<Key>) -> Option<&LRUNode<Key, Value>> {
        self.map.get(key)
    }

    fn get_node(&self, key: &Option<Key>) -> Result<&LRUNode<Key, Value>, SbroadError> {
        self.map
            .get(key)
            .ok_or_else(|| SbroadError::NotFound(Entity::Node, format!("(LRU) with key {key:?}")))
    }

    fn get_node_mut(&mut self, key: &Option<Key>) -> Result<&mut LRUNode<Key, Value>, SbroadError> {
        self.map.get_mut(key).ok_or_else(|| {
            SbroadError::NotFound(Entity::Node, format!("(mutable LRU) with key {key:?}"))
        })
    }

    fn add_first(&mut self, key: Key, value: Value) -> Result<(), SbroadError> {
        let new_node = LRUNode::new(value);
        self.map.insert(Some(key.clone()), new_node);
        self.size += 1;
        let head_node = self.get_node(&None)?;
        let head_next_id = head_node.next.clone();
        self.link_node(key, &None, &head_next_id)?;
        Ok(())
    }

    fn make_first(&mut self, key: &Key) -> Result<(), SbroadError> {
        self.unlink_node(&Some(key.clone()))?;
        let head_node = self.get_node(&None)?;
        let head_next_id = head_node.next.clone();
        self.link_node(key.clone(), &None, &head_next_id)?;
        Ok(())
    }

    fn is_first(&self, key: &Key) -> Result<bool, SbroadError> {
        let head_node = self.get_node(&None)?;
        Ok(head_node.next == Some(key.clone()))
    }

    fn link_node(
        &mut self,
        key: Key,
        prev: &Option<Key>,
        next: &Option<Key>,
    ) -> Result<(), SbroadError> {
        let node = self.get_node_mut(&Some(key.clone()))?;
        node.replace_prev(prev.clone());
        node.replace_next(next.clone());
        let prev_node = self.get_node_mut(prev)?;
        prev_node.replace_next(Some(key.clone()));
        let next_node = self.get_node_mut(next)?;
        next_node.replace_prev(Some(key));
        Ok(())
    }

    fn unlink_node(&mut self, key: &Option<Key>) -> Result<(), SbroadError> {
        // We don't want to remove sentinel.
        if key.is_none() {
            return Ok(());
        }

        let node = self.get_node_mut(key)?;
        let prev_id = node.prev.take();
        let next_id = node.next.take();
        let prev_node = self.get_node_mut(&prev_id)?;
        prev_node.replace_next(next_id.clone());
        let next_node = self.get_node_mut(&next_id)?;
        next_node.replace_prev(prev_id);
        Ok(())
    }

    fn remove_last(&mut self) -> Result<(), SbroadError> {
        let head_node = self.get_node(&None)?;
        let head_prev_id = head_node.prev.clone();
        if head_prev_id.is_none() {
            return Ok(());
        }

        let map = &mut self.map;
        if let Some(evict_fn) = &self.evict_fn {
            let head_prev = map.get_mut(&head_prev_id).ok_or_else(|| {
                SbroadError::NotFound(
                    Entity::Node,
                    format!("(mutable LRU) with key {:?}", &head_prev_id),
                )
            })?;
            evict_fn(&mut head_prev.value)?;
        }

        self.unlink_node(&head_prev_id)?;
        if let Some(last_key) = &head_prev_id {
            self.map.remove(&Some(last_key.clone()));
            self.size -= 1;
        }
        Ok(())
    }
}

impl<Key, Value> Cache<Key, Value> for LRUCache<Key, Value>
where
    Value: Default + Debug,
    Key: Clone + Eq + std::hash::Hash + std::fmt::Debug,
{
    fn new(capacity: usize, evict_fn: Option<EvictFn<Value>>) -> Result<Self, SbroadError> {
        if capacity == 0 {
            return Err(SbroadError::Invalid(
                Entity::Cache,
                Some("LRU cache capacity must be greater than zero".to_string()),
            ));
        }
        let head = LRUNode::sentinel();
        let mut map: HashMap<Option<Key>, LRUNode<Key, Value>> =
            HashMap::with_capacity(capacity + 2);
        map.insert(None, head);

        Ok(LRUCache {
            capacity,
            size: 0,
            map,
            evict_fn,
        })
    }

    fn get(&mut self, key: &Key) -> Result<Option<&Value>, SbroadError> {
        if self.get_node_or_none(&Some(key.clone())).is_none() {
            return Ok(None);
        }

        if self.is_first(key)? {
            let value = &self.get_node(&Some(key.clone()))?.value;
            return Ok(Some(value));
        }
        self.make_first(key)?;

        if let Some(node) = self.get_node_or_none(&Some(key.clone())) {
            Ok(Some(&node.value))
        } else {
            Err(SbroadError::FailedTo(
                Action::Retrieve,
                Some(Entity::Value),
                format!("from the LRU cache for a key {key:?}"),
            ))
        }
    }

    fn put(&mut self, key: Key, value: Value) -> Result<(), SbroadError> {
        if let Entry::Occupied(mut entry) = self.map.entry(Some(key.clone())) {
            let node = entry.get_mut();
            node.value = value;
            self.make_first(&key)?;
            return Ok(());
        }

        self.add_first(key, value)?;
        if self.size > self.capacity {
            self.remove_last()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
