use super::*;
use std::boxed::Box;

fn verify(tree: &Tree, live: &[bool; 7]) {
    fn walk(
        tree: &Tree,
        slot: Option<Slot>,
        parent: Option<Slot>,
        seen: &mut [bool; 7],
        low: usize,
        high: usize,
    ) -> usize {
        let Some(slot) = slot else {
            return 1;
        };
        let index = slot.index();
        assert!(low <= index && index < high);
        assert!(!core::mem::replace(&mut seen[index], true));
        let link = tree.link(slot);
        assert_eq!(link.parent, parent);
        if link.color == Color::Red {
            assert!(!tree.red(link.left));
            assert!(!tree.red(link.right));
        }
        let left = walk(tree, link.left, Some(slot), seen, low, index);
        let right = walk(tree, link.right, Some(slot), seen, index + 1, high);
        assert_eq!(left, right);
        left + usize::from(link.color == Color::Black)
    }

    assert!(!tree.red(tree.root()));
    let mut seen = [false; 7];
    walk(tree, tree.root(), None, &mut seen, 0, 7);
    assert_eq!(&seen, live);
    assert_eq!(tree.length, live.iter().filter(|&&value| value).count());
    assert_eq!(tree.is_empty(), live.iter().all(|&value| !value));
    assert_eq!(tree.first().map(Slot::index), live.iter().position(|&v| v));
}

#[test]
fn insertion_and_exact_removal_preserve_nodes_and_black_height() {
    // Exercise every insertion order and every cyclic order of removal.
    fn permutations(tree: &mut Tree, order: &mut [usize; 7], index: usize) {
        if index != order.len() {
            for next in index..order.len() {
                order.swap(index, next);
                permutations(tree, order, index + 1);
                order.swap(index, next);
            }
            return;
        }
        for offset in 0..order.len() {
            let mut live = [false; 7];
            for &key in order.iter() {
                let position = tree.prepare(|slot| key.cmp(&slot.index())).unwrap();
                tree.insert(Slot::new(key).unwrap(), position);
                live[key] = true;
                verify(tree, &live);
            }
            assert!(tree.prepare(|slot| 3usize.cmp(&slot.index())).is_err());
            for step in 0..order.len() {
                let key = order[(offset + step) % order.len()];
                tree.remove(Slot::new(key).unwrap());
                live[key] = false;
                verify(tree, &live);
            }
        }
    }
    let links = Box::leak(Box::new([Link::EMPTY; 7]));
    let mut tree = Tree::new(links);
    permutations(&mut tree, &mut [0, 1, 2, 3, 4, 5, 6], 0);
}
