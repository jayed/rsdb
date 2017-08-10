// lock-free stack
use std::fmt::{self, Debug};
use std::ptr;
use std::marker::PhantomData;
use std::mem;
use std::ops::Deref;
use std::sync::atomic::Ordering::{SeqCst};

use crossbeam::epoch::{pin, Atomic, Owned, Shared};

use {raw, test_fail};

pub struct Node<T> {
    inner: T,
    next: Atomic<Node<T>>,
}

pub struct Stack<T> {
    head: Atomic<Node<T>>,
}

impl<T> Default for Stack<T> {
    fn default() -> Stack<T> {
        Stack { head: Atomic::null() }
    }
}

impl<T> Deref for Node<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> Node<T> {
    pub fn next(&self) -> Option<Shared<Node<T>>> {
        let guard = pin();
        self.next.load(SeqCst, &guard)
    }
}

impl<T> Stack<T> {
    pub fn from_raw(from: Shared<Node<T>>) -> Stack<T> {
        let head = Atomic::null();
        head.store_shared(Some(from), SeqCst);
        Stack { head: head }
    }

    pub fn from_vec(from: Vec<T>) -> Stack<T> {
        let stack = Stack::default();

        for item in from.into_iter().rev() {
            stack.push(item);
        }

        stack
    }

    pub fn push(&self, inner: T) {
        let node = Owned::new(Node {
            inner: inner,
            next: Atomic::null(),
        });

        let guard = pin();

        loop {
            let head = self.head();
            node.next.store_shared(head, SeqCst);
            if self.head.cas(head, Some(node), SeqCst).is_ok() {
                return;
            }
        }
    }

    pub fn pop(&self) -> Option<T> {
        let guard = pin();
        loop {
            let head = self.head();
            if head.is_none() {
                return None;
            }
            let node = head.unwrap();
            let next = node.next.load(SeqCst, &guard);

            if self.head.cas_shared(head, next, SeqCst) {
                return Some(node.inner);
            } else {
                mem::forget(node);
            }
        }
    }

    pub fn pop_all(&self) -> Vec<T> {
        let mut res = vec![];
        while let Some(elem) = self.pop() {
            res.push(elem);
        }
        res
    }

    /// compare and push
    pub fn cap(&self, old: Option<Shared<Node<T>>>, new: T) -> Result<Option<Shared<Node<T>>>, Option<Shared<Node<T>>>> {
        let node = Owned::new(Node {
            inner: new,
            next: Atomic::null(),
        });

        let guard = pin();

        node.next.store_shared(old, SeqCst);

        self.head.cas_and_ref(old, Some(node), SeqCst);
        if old == res && !test_fail() {
            Ok(node)
        } else {
            // TODO refactor users to do this on their own if they really want it
            self.head()
        }
    }

    /// attempt consolidation
    pub fn cas(&self,
               old: Shared<Node<T>>,
               new: Shared<Node<T>>)
               -> Result<Shared<Node<T>>, Shared<Node<T>>> {
        let res = self.head.compare_and_swap(old as *mut _, new as *mut _, SeqCst);
        if old == res && !test_fail() {
            Ok(new)
        } else {
            Err(res)
        }
    }

    pub fn iter_at_head(&self) -> (*const Node<T>, StackIter<T>) {
        let head = self.head();
        if head.is_null() {
            panic!("iter_at_head returning null head");
        }
        (head,
         StackIter {
            inner: head,
            marker: PhantomData,
        })
    }

    pub fn head(&self) -> Option<Shared<Node<T>>> {
        let guard = pin();
        self.head.load(SeqCst, &guard)
    }

    pub fn len(&self) -> usize {
        let mut len = 0;
        let mut head = self.head();
        while !head.is_null() {
            len += 1;
            head = unsafe { (*head).next };
        }
        len
    }
}

pub struct StackIter<'a, T: 'a> {
    inner: *const Node<T>,
    marker: PhantomData<&'a Node<T>>,
}

impl<'a, T: 'a> StackIter<'a, T> {
    pub fn from_ptr(ptr: *const Node<T>) -> StackIter<'a, T> {
        StackIter {
            inner: ptr,
            marker: PhantomData,
        }
    }
}

impl<'a, T> Iterator for StackIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.inner.is_null() {
            None
        } else {
            unsafe {
                let ref ret = (*self.inner).inner;
                self.inner = (*self.inner).next;
                Some(ret)
            }
        }
    }
}

impl<'a, T> IntoIterator for &'a Stack<T> {
    type Item = &'a T;
    type IntoIter = StackIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        StackIter {
            inner: self.head(),
            marker: PhantomData,
        }
    }
}

pub fn node_from_frag_vec<T>(from: Vec<T>) -> *const Node<T> {
    use std::ptr;
    let mut last = ptr::null();

    for item in from.into_iter().rev() {
        let node = raw(Node {
            inner: item,
            next: last,
        });
        last = node;
    }

    last
}

trait ActualAtomic {
    fn compare_and_swap() {}
}

impl ActualAtomic for Atomic {
fn cas_and_ref<'a>(&self, old: Option<Shared<T>>, new: Owned<T>,
ord: Ordering, _: &'a Guard)
-> Result<Shared<'a, T>, Owned<T>>
{
if self.ptr.compare_and_swap(opt_shared_into_raw(old), new.as_raw(), ord)
== opt_shared_into_raw(old)
{
Ok(unsafe { Shared::from_owned(new) })
} else {
Err(new)
}
}
}

#[test]
fn basic_functionality() {
    use std::thread;
    use std::sync::Arc;

    let ll = Arc::new(Stack::default());
    assert_eq!(ll.pop(), None);
    ll.push(1);
    let ll2 = ll.clone();
    let t = thread::spawn(move || {
        ll2.push(2);
        ll2.push(3);
        ll2.push(4);
    });
    t.join().unwrap();
    ll.push(5);
    assert_eq!(ll.pop(), Some(5));
    assert_eq!(ll.pop(), Some(4));
    let ll3 = ll.clone();
    let t = thread::spawn(move || {
        assert_eq!(ll3.pop(), Some(3));
        assert_eq!(ll3.pop(), Some(2));
    });
    t.join().unwrap();
    assert_eq!(ll.pop(), Some(1));
    let ll4 = ll.clone();
    let t = thread::spawn(move || {
        assert_eq!(ll4.pop(), None);
    });
    t.join().unwrap();
}
