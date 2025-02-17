use super::*;

/// Based on the free-space-map allocator from postgresql:
/// https://github.com/postgres/postgres/blob/02ed3c2bdcefab453b548bc9c7e0e8874a502790/src/backend/storage/freespace/README
pub struct FirstFitTree {
    data: BitArr!(for SEGMENT_SIZE, in u8, Lsb0),
    leaves: Vec<(u32, u32)>, // Leaf Nodes: Offset, Size
    tree: Vec<u32>,          // Internal tree nodes: max free size in subtree
    tree_height: u32,
}

impl Allocator for FirstFitTree {
    fn data(&mut self) -> &mut BitArr!(for SEGMENT_SIZE, in u8, Lsb0) {
        &mut self.data
    }

    /// Constructs a new `FirstFitFSM` given the segment allocation bitmap.
    /// The `bitmap` must have a length of `SEGMENT_SIZE`.
    fn new(bitmap: [u8; SEGMENT_SIZE_BYTES]) -> Self {
        let data = BitArray::new(bitmap);
        let mut allocator = FirstFitTree {
            data,
            leaves: Vec::new(),
            tree: Vec::new(),
            tree_height: 0,
        };
        allocator.build_fsm_tree();
        allocator
    }

    fn allocate(&mut self, size: u32) -> Option<u32> {
        if size == 0 {
            return Some(0);
        }

        // empty tree or not enough space
        if self.tree.is_empty() || self.tree[0] < size {
            return None; // Not enough free space
        }

        // only one leaf
        if self.tree_height == 0 && self.tree.len() == 1 {
            let offset = self.leaves[0].0;

            self.leaves[0].0 += size;
            self.leaves[0].1 -= size;
            self.tree[0] -= size;

            self.mark(offset, size, Action::Allocate);
            return Some(offset);
        }

        let mut current_node_index = 0;
        loop {
            let left_child_index = 2 * current_node_index + 1;
            let right_child_index = 2 * current_node_index + 2;

            if left_child_index >= self.tree.len() {
                // We've reached the bottom of the *internal* tree.
                break;
            }

            // Check left child first for first fit
            if let Some(left_child_value) = self.tree.get(left_child_index) {
                if *left_child_value >= size {
                    current_node_index = left_child_index;
                    continue;
                }
            }
            if let Some(right_child_value) = self.tree.get(right_child_index) {
                if *right_child_value >= size {
                    current_node_index = right_child_index;
                    continue;
                }
            }
            unreachable!();
        }

        // Map internal node index to the leaves vector index
        let conceptual_leaf_start = self.tree.len() + 1;
        let leaf_index_in_leaves = (current_node_index + 1) * 2 - conceptual_leaf_start;

        let offset = self.leaves[leaf_index_in_leaves].0;

        self.mark(offset, size, Action::Allocate);

        // Update the tree
        self.leaves[leaf_index_in_leaves].0 += size;
        self.leaves[leaf_index_in_leaves].1 -= size;
        self.update_tree_after_leaf_change(leaf_index_in_leaves);

        return Some(offset);
    }

    fn allocate_at(&mut self, size: u32, offset: u32) -> bool {
        // Because the tree is sorted by offset because of how it's build, this shouldn't be to
        // hard to implement efficiently
        todo!()
    }
}

impl FirstFitTree {
    fn get_free_segments(&mut self) -> Vec<(u32, u32)> {
        let mut offset: u32 = 0;
        let mut free_segments = Vec::new();
        while offset < SEGMENT_SIZE as u32 {
            if !self.data()[offset as usize] {
                // If bit is 0, it's free
                let start_offset = offset;
                let mut current_size: u32 = 0;
                while offset < SEGMENT_SIZE as u32 && !self.data()[offset as usize] {
                    current_size += 1;
                    offset += 1;
                }
                free_segments.push((start_offset, current_size));
            } else {
                offset += 1;
            }
        }
        free_segments
    }

    fn build_fsm_tree(&mut self) {
        self.leaves = self.get_free_segments();
        let leaf_nodes_num = self.leaves.len();

        if leaf_nodes_num <= 1 {
            self.tree_height = 0;
            self.tree.clear(); // No internal nodes if 0 or 1 leaf
            if leaf_nodes_num == 1 {
                self.tree.push(self.leaves[0].1); // Root = size of the single leaf
            }
            return;
        }

        // Calculate tree height and total internal nodes for a *nearly* complete tree
        self.tree_height = (leaf_nodes_num as f64).log2().ceil() as u32;
        // Internal nodes in a *complete* tree of height tree_height-1
        let internal_nodes_num = (1 << self.tree_height) - 1;

        self.tree.clear();
        self.tree.resize(internal_nodes_num as usize, 0);

        // Initialize the last level of internal nodes from leaves
        let last_level_start_index = internal_nodes_num / 2;
        for i in (0..internal_nodes_num).rev() {
            let left_child_index = 2 * i + 1;
            let right_child_index = 2 * i + 2;

            if i >= last_level_start_index {
                // Last level internal nodes: map to leaves directly
                let leaf_start_index = i - last_level_start_index; // Leaf index offset

                let left_leaf_val = self
                    .leaves
                    .get(leaf_start_index * 2)
                    .map_or(0, |&(_, size)| size);
                let right_leaf_val = self
                    .leaves
                    .get(leaf_start_index * 2 + 1)
                    .map_or(0, |&(_, size)| size); // May be out of bounds

                self.tree[i] = std::cmp::max(left_leaf_val, right_leaf_val);
            } else {
                // Higher level internal nodes: aggregate from children in `tree`
                let left_child_value = *self.tree.get(left_child_index).unwrap_or(&0);
                let right_child_value = *self.tree.get(right_child_index).unwrap_or(&0);
                self.tree[i] = std::cmp::max(left_child_value, right_child_value);
            }
        }
    }

    fn update_tree_after_leaf_change(&mut self, leaf_index: usize) {
        // Calculate the index of the corresponding internal node in the last level
        let mut current_index = self.tree.len() / 2 + leaf_index / 2;

        // Update that internal node based on its *current* children (which might be leaves or
        // other internal nodes)
        loop {
            let left_child_index = 2 * current_index + 1;
            let right_child_index = 2 * current_index + 2;

            let left_child_value;
            let right_child_value;

            if current_index >= self.tree.len() / 2 {
                // We are at the last internal level, children are leaves
                let leaf_base_index = (current_index - self.tree.len() / 2) * 2;
                left_child_value = self
                    .leaves
                    .get(leaf_base_index)
                    .map_or(0, |&(_, size)| size);
                right_child_value = self
                    .leaves
                    .get(leaf_base_index + 1)
                    .map_or(0, |&(_, size)| size);
            } else {
                // Children are internal nodes
                left_child_value = *self.tree.get(left_child_index).unwrap_or(&0);
                right_child_value = *self.tree.get(right_child_index).unwrap_or(&0);
            }

            let new_parent_value = std::cmp::max(left_child_value, right_child_value);

            if self.tree[current_index] == new_parent_value {
                // No further update needed if parent value is unchanged
                return;
            }
            self.tree[current_index] = new_parent_value;

            if current_index == 0 {
                // Reached root
                return;
            }
            current_index = (current_index - 1) / 2; // Move up to the parent
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_empty() {
        let bitmap = [0u8; SEGMENT_SIZE_BYTES];
        let allocator = FirstFitTree::new(bitmap);

        // In an empty bitmap, the root node should have a large free space
        assert_eq!(allocator.tree[0], SEGMENT_SIZE as u32);
        assert_eq!(allocator.tree_height, 0);
        assert_eq!(allocator.tree.len(), 1); // Now root is the only node
        assert_eq!(allocator.leaves[0], (0, SEGMENT_SIZE as u32));
    }

    #[test]
    fn build_simple() {
        // Example bitmap: 3 segments allocated at the beginning, 2 free, 3 allocated, rest free
        let mut allocator = FirstFitTree::new([0u8; SEGMENT_SIZE_BYTES]);
        let bitmap = allocator.data();

        // Manually allocate some segments
        bitmap[0..3].fill(true); // Allocate 3 blocks at the beginning
        bitmap[5..7].fill(true); // Allocate 2 blocks after the free ones

        let allocator = FirstFitTree::new(bitmap.into_inner());

        // binary heap layout
        let tree = vec![SEGMENT_SIZE as u32 - 7];

        assert_eq!(allocator.tree, tree);
        assert_eq!(allocator.tree_height, 1);
        assert_eq!(allocator.tree.len(), 1); // Only root node now
        assert_eq!(allocator.leaves.len(), 2);
        assert_eq!(allocator.leaves[0], (3, 2));
        assert_eq!(allocator.leaves[1], (7, SEGMENT_SIZE as u32 - 7));
    }

    #[test]
    fn build_complex() {
        let mut allocator = FirstFitTree::new([0u8; SEGMENT_SIZE_BYTES]);
        let bitmap = allocator.data();

        // Manually allocate some segments to create a non-trivial tree
        bitmap[0..3].fill(true);
        bitmap[5..8].fill(true);
        bitmap[8..10].fill(true);
        bitmap[14..22].fill(true);
        bitmap[35..36].fill(true);
        bitmap[42..53].fill(true);

        let allocator = FirstFitTree::new(bitmap.into_inner());

        // binary heap layout
        let tree = vec![
            SEGMENT_SIZE as u32 - 53,
            //
            13,
            SEGMENT_SIZE as u32 - 53,
            //
            4,
            13,
            SEGMENT_SIZE as u32 - 53,
            0,
        ];

        assert_eq!(allocator.tree_height, 3);
        assert_eq!(allocator.leaves.len(), 5);
        assert_eq!(allocator.leaves[0], (3, 2));
        assert_eq!(allocator.leaves[1], (10, 4));
        assert_eq!(allocator.leaves[2], (22, 13));
        assert_eq!(allocator.leaves[3], (36, 6));
        assert_eq!(allocator.leaves[4], (53, SEGMENT_SIZE as u32 - 53));
        assert_eq!(tree, allocator.tree);
    }

    #[test]
    fn allocate_empty_fsm_tree() {
        let bitmap = [0u8; SEGMENT_SIZE_BYTES];
        let mut allocator = FirstFitTree::new(bitmap);

        let allocation = allocator.allocate(1024);
        assert!(allocation.is_some()); // Allocation should succeed

        let allocated_offset = allocation.unwrap();
        assert_eq!(allocated_offset, 0); // Should allocate at the beginning

        // Check if the allocated region is marked as used in the bitmap
        assert!(allocator.data()[0..1024 as usize].all());
        // Check root node value after allocation
        assert_eq!(allocator.tree[0], SEGMENT_SIZE as u32 - 1024);
    }

    #[test]
    fn allocate_complex_fsm_tree() {
        let mut allocator = FirstFitTree::new([0u8; SEGMENT_SIZE_BYTES]);
        let bitmap = allocator.data();

        // Manually allocate some segments to create a non-trivial tree
        bitmap[0..3].fill(true);
        bitmap[5..8].fill(true);
        bitmap[8..10].fill(true);
        bitmap[14..22].fill(true);
        bitmap[35..36].fill(true);
        bitmap[42..53].fill(true);

        let mut allocator = FirstFitTree::new(bitmap.into_inner());

        // First should allocate from the segment at offset 3 with size 2
        let allocation = allocator.allocate(2); // Request allocation of size 2
        assert!(allocation.is_some());
        assert_eq!(allocation.unwrap(), 3);
        // Verify that the allocated region is marked in the bitmap
        assert!(allocator.data()[3..5].all());

        let allocation2 = allocator.allocate(10);
        assert!(allocation2.is_some());
        assert_eq!(allocation2.unwrap(), 22);
        assert!(allocator.data()[22..32].all());

        // Allocate again, to use the next first fit segment
        let allocation2 = allocator.allocate(100);
        assert!(allocation2.is_some());
        assert_eq!(allocation2.unwrap(), 53);
        assert!(allocator.data()[53..153].all());
        assert_eq!(allocator.tree[0], SEGMENT_SIZE as u32 - 153);
    }

    #[test]
    fn allocate_fail_fsm_tree() {
        let mut allocator = FirstFitTree::new([0u8; SEGMENT_SIZE_BYTES]);
        let root_free_space = allocator.tree[0];

        // Try to allocate more than available space
        let allocation = allocator.allocate(root_free_space + 1);
        assert!(allocation.is_none()); // Allocation should fail

        // Check if fsm_tree root value is still the same
        assert_eq!(allocator.tree[0], root_free_space); // Should remain unchanged
    }
}
