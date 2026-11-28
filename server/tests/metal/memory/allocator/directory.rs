use super::*;
use crate::metal::index::Color;

pub(in crate::metal::memory::allocator) fn verify(directory: &Directory, regions: &[Region]) {
    fn walk(
        directory: &Directory,
        regions: &[Region],
        seen: &mut [bool],
        previous: &mut Option<PhysicalRange>,
        total: &mut ByteLength,
        node: Option<Slot>,
        parent: Option<Slot>,
    ) -> usize {
        let Some(node) = node else {
            return 1;
        };
        assert!(!core::mem::replace(&mut seen[node.index()], true));
        let link = directory.tree.link(node);
        assert_eq!(link.parent, parent);
        if link.color == Color::Red {
            assert!(!directory.tree.red(link.left));
            assert!(!directory.tree.red(link.right));
        }
        let left = walk(
            directory,
            regions,
            seen,
            previous,
            total,
            link.left,
            Some(node),
        );
        let range = regions[node.index()].description().physical;
        assert!(previous.is_none_or(|prior| prior.end() <= range.start()));
        *previous = Some(range);
        *total = total.checked_add(range.length()).unwrap();
        let right = walk(
            directory,
            regions,
            seen,
            previous,
            total,
            link.right,
            Some(node),
        );
        assert_eq!(left, right, "black heights");
        left + usize::from(link.color == Color::Black)
    }

    assert!(!directory.tree.red(directory.tree.root()));
    let mut seen = [false; 6];
    let seen = &mut seen[..regions.len()];
    let mut total = ByteLength::new(0);
    walk(
        directory,
        regions,
        seen,
        &mut None,
        &mut total,
        directory.tree.root(),
        None,
    );
    assert!(seen.iter().all(|&value| value));
    assert_eq!(directory.managed, total);
}
