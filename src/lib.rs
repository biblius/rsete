mod display;
mod id;
mod item;
mod node;

use crate::{
    item::AlphaMemoryItem,
    node::{BetaMemoryNode, JoinNode, ProductionNode},
};
use item::{Condition, ConstantTest, TestAtJoinNode, Token, Wme};
use node::{AlphaMemoryNode, Node, Production};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

type RcCell<T> = Rc<RefCell<T>>;

trait IntoCell: Sized {
    fn to_cell(self) -> RcCell<Self> {
        Rc::new(RefCell::new(self))
    }
}

trait IntoNodeCell: Sized {
    fn to_node_cell(self) -> RcCell<Node>;
}

#[derive(Debug)]
pub struct Rete {
    /// Token tree root
    dummy_top_token: RcCell<Token>,

    /// Beta network root
    dummy_top_node: RcCell<Node>,

    /// Since every WME is represented as a triple, we only need to do 8 hash table look ups whenever one is added to the
    /// network to find possibly matching constant tests for it. This removes the need for a constant test network.
    constant_tests: HashMap<ConstantTest, RcCell<AlphaMemoryNode>>,

    /// Maps WME IDs to their corresponding elements for quick removal of tokens
    working_memory: HashMap<usize, RcCell<Wme>>,

    /// Maps WME IDs to Alpha Nodes that contain items which hold the WME
    wme_alphas: HashMap<usize, Vec<RcCell<AlphaMemoryNode>>>,
}

impl Rete {
    fn new() -> Self {
        let dummy_top_node = BetaMemoryNode::new(None);
        println!("Created initial dummy {dummy_top_node}");
        let dummy_top_node = dummy_top_node.to_node_cell();

        let dummy_top_token = Token::new(&dummy_top_node, None, &Wme::new([0, 0, 0]).to_cell());

        dummy_top_node.borrow_mut().add_token(&dummy_top_token);

        Self {
            constant_tests: HashMap::new(),
            wme_alphas: HashMap::new(),
            working_memory: HashMap::new(),
            dummy_top_node,
            dummy_top_token,
        }
    }

    fn add_wme(&mut self, elements: [usize; 3]) -> usize {
        let wme = Wme::new(elements);
        let id = wme.id;

        println!("Adding WME {:?}", wme);

        for element in wme.permutations() {
            let Some(memory) = self.constant_tests.get(&element) else { continue };

            // Index the memories that will hold this WME by its ID
            if let Some(memories) = self.wme_alphas.get_mut(&id) {
                memories.push(Rc::clone(memory));
            } else {
                self.wme_alphas.insert(id, vec![Rc::clone(memory)]);
            }

            let wme = wme.to_cell();
            self.working_memory.insert(id, Rc::clone(&wme));

            println!(
                "Found existing memory {} for element {:?}",
                memory.borrow().id,
                element
            );

            activate_alpha_memory(memory, &wme);

            return id;
        }

        // Ensure the WME has a corresponding Alpha Node in advance if none is found
        println!("No memory found for WME {wme:?}");

        let wme = wme.to_cell();
        self.working_memory.insert(id, Rc::clone(&wme));

        id
    }

    fn remove_wme(&mut self, id: usize) {
        println!("Removing WME {id}");

        if let Some(memories) = self.wme_alphas.remove(&id) {
            for memory in memories {
                println!("Removing WME {id} from alpha memory {}", memory.borrow().id);
                let memory = &mut *memory.borrow_mut();
                memory
                    .items
                    .retain(|item| item.borrow().wme.borrow().id != id);
            }
        }

        if let Some(wme) = self.working_memory.remove(&id) {
            println!("Removing WME {} from working memory", wme.borrow());

            let mut wme = wme.borrow_mut();
            let mut tokens = std::mem::take(&mut wme.tokens);

            drop(wme);

            while let Some(token) = tokens.pop() {
                Token::delete_self_and_descendants(token)
            }
        }
    }

    fn build_or_share_alpha_memory_node(
        &mut self,
        condition: &Condition,
    ) -> RcCell<AlphaMemoryNode> {
        let constant_test = ConstantTest::from(*condition);

        println!("Searching for constant test {constant_test:?}");

        // Check whether an alpha memory like this exists
        if let Some(alpha_mem) = self.constant_tests.get(&constant_test) {
            println!("Shared {}", alpha_mem.borrow());
            return Rc::clone(alpha_mem);
        }

        // Alpha memory not found, create new one and insert into map
        let am = AlphaMemoryNode::new().to_cell();

        self.constant_tests.insert(constant_test, Rc::clone(&am));

        println!(
            "Inserting constant test {:?} for AM {}",
            constant_test,
            am.borrow().id
        );

        for (_, wme) in self.working_memory.iter() {
            let _wme = wme.borrow();
            if constant_test.matches(&_wme) {
                let a_mems = self
                    .wme_alphas
                    .entry(_wme.id)
                    .or_insert_with(|| vec![Rc::clone(&am)]);
                a_mems
                    .iter()
                    .for_each(|mem| activate_alpha_memory(mem, wme))
            }
        }

        am
    }

    fn add_production(&mut self, production: Production) {
        println!("Adding production {}", production.id);
        let conditions = &production.conditions;

        assert!(!conditions.is_empty(), "LHS of production cannot be empty");

        println!("Processing condition {:?}", conditions[0]);

        let mut current_node = Rc::clone(&self.dummy_top_node);

        let mut earlier_conds = vec![];

        let mut tests = get_join_tests_from_condition(&conditions[0], &earlier_conds);
        let mut alpha_memory = self.build_or_share_alpha_memory_node(&conditions[0]);
        current_node = build_or_share_join_node(&current_node, &alpha_memory, tests);

        for i in 1..conditions.len() {
            println!("Processing condition {:?}", conditions[i]);
            // Get the beta memory node Mi
            current_node = build_or_share_beta_memory_node(&current_node);

            earlier_conds.push(conditions[i - 1]);

            // Get the join node Ji for conition Ci
            tests = get_join_tests_from_condition(&conditions[i], &earlier_conds);
            alpha_memory = self.build_or_share_alpha_memory_node(&conditions[i]);
            current_node = build_or_share_join_node(&current_node, &alpha_memory, tests);
        }

        let production = ProductionNode::new(production, &current_node).to_node_cell();

        current_node.borrow_mut().add_child(&production);

        update_new_node_with_matches_from_above(&production);
    }
}

fn update_new_node_with_matches_from_above(node: &RcCell<Node>) {
    println!("Updating node {}", node.borrow());

    let Some(parent) = node.borrow().parent() else { return; };

    println!("Updating parent {}", parent.borrow());

    let children = match *parent.borrow_mut() {
        Node::Beta(ref beta) => {
            beta.items
                .iter()
                .for_each(|token| activate_left(node, token, &token.borrow().wme));
            return;
        }
        Node::Join(ref mut join) => std::mem::replace(&mut join.children, vec![Rc::clone(node)]),
        Node::Production(_) => panic!("Production node cannot have children"),
    };

    // This avoids keeping the mutable borrow when recursively activating
    {
        let Node::Join(ref join) = *parent.borrow() else { return; };
        join.alpha_memory
            .borrow()
            .items
            .iter()
            .for_each(|item| activate_right(&parent, &item.borrow().wme));
    }

    let Node::Join(ref mut join) = *parent.borrow_mut() else { return; };
    join.children = children;
}

/// Activation of alpha memories cause them to right activate join nodes which in turn makes
/// the join nodes search through their beta memories and perform matches on already existing tokens,
/// further propagating left activations if they succeed.
fn activate_alpha_memory(alpha_mem_node: &RcCell<AlphaMemoryNode>, wme: &RcCell<Wme>) {
    let item = AlphaMemoryItem::new(wme, alpha_mem_node).to_cell();

    // Insert new item at the head of the node's items
    let mut alpha_mem = alpha_mem_node.borrow_mut();
    alpha_mem.items.push(Rc::clone(&item));

    println!("Activating Alpha Node: {alpha_mem}");

    alpha_mem
        .successors
        .iter()
        .for_each(|node| activate_right(node, wme))
}

/// Left activation of Beta nodes cause them to create tokens and propagate the left activation to their children, i.e.
/// Join nodes, with the newly created token.
///
/// Left activation of Join nodes cause them to execute their join tests with the given token and if successful propagate
/// the left activation to their children.
///
/// Left activation of production nodes cause them to activate the underlying production.
fn activate_left(node: &RcCell<Node>, parent_token: &RcCell<Token>, wme: &RcCell<Wme>) {
    match &mut *node.borrow_mut() {
        Node::Beta(ref mut beta_node) => {
            let new_token = Token::new(node, Some(Rc::downgrade(parent_token)), wme);
            println!(
                "Left activating beta {} and appending token {}",
                beta_node.id,
                new_token.borrow().id
            );
            beta_node.items.push(Rc::clone(&new_token));
            beta_node
                .children
                .iter()
                .for_each(|child| activate_left(child, &new_token, wme));
        }
        Node::Join(ref mut join_node) => {
            println!("Left activating join {}", join_node.id);
            for alpha_mem_item in join_node.alpha_memory.borrow().items.iter() {
                let test = join_test(
                    &join_node.tests,
                    parent_token,
                    &alpha_mem_item.borrow().wme.borrow(),
                );
                if test {
                    join_node.children.iter().for_each(|child| {
                        activate_left(child, parent_token, &alpha_mem_item.borrow().wme)
                    });
                }
            }
        }
        Node::Production(p_node) => {
            println!(
                "====================\nProduction node activated! {p_node}\n===================="
            )
        }
    }
}

/// A right activation of a `JoinNode` will cause it to iterate through its
/// parent's tokens and perform a [join_test] on each one and the given WME.
///
/// For every test that passes, a left activation is triggered on each of the node's
/// children.
///
/// Right activations are caused by [AlphaMemoryNode]s when [WME][Wme]s are changed or
/// when new [WME][Wme]s enter the network
fn activate_right(node: &RcCell<Node>, wme: &RcCell<Wme>) {
    let node = &*node.borrow();
    println!("Right activating {} {}", node._type(), node.id());
    match node {
        Node::Join(ref join_node) => {
            if let Node::Beta(ref parent) = *join_node.parent.borrow() {
                for token in parent.items.iter() {
                    let test = join_test(&join_node.tests, token, &wme.borrow());
                    if test {
                        join_node
                            .children
                            .iter()
                            .for_each(|child| activate_left(child, token, wme))
                    }
                }
            }
        }
        Node::Beta(_) => unreachable!("Beta memory nodes are never right activated"),
        Node::Production(_) => unreachable!("Production nodes are never right activated"),
    }
}

fn build_or_share_beta_memory_node(parent: &RcCell<Node>) -> RcCell<Node> {
    if let Some(children) = parent.borrow().children() {
        // Look for an existing beta node to share
        for child in children {
            if let Node::Beta(ref beta) = *child.borrow() {
                println!("Shared {beta}");
                return Rc::clone(child);
            }
        }
    }

    let new = BetaMemoryNode::new(Some(Rc::clone(parent))).to_node_cell();

    parent.borrow_mut().add_child(&new);

    update_new_node_with_matches_from_above(&new);

    println!("Built {}", new.borrow());

    new
}

fn build_or_share_join_node(
    parent: &RcCell<Node>,
    alpha_memory: &RcCell<AlphaMemoryNode>,
    tests: Vec<TestAtJoinNode>,
) -> RcCell<Node> {
    if let Some(children) = parent.borrow().children() {
        // Look for an existing join node to share
        for child in children {
            if let Node::Join(node) = &*child.borrow() {
                println!("TESTS {}", node.tests.as_slice() == tests);
                println!(
                    "OVERFLOW {}",
                    *node.alpha_memory.borrow() == *alpha_memory.borrow()
                );
                if node.tests.as_slice() == tests
                    && *node.alpha_memory.borrow() == *alpha_memory.borrow()
                {
                    println!("Sharing {node}");
                    return Rc::clone(child);
                }
            }
        }
    }

    let new = JoinNode::new(parent, alpha_memory, tests).to_node_cell();

    // Add the newly created node to the alpha memory successors
    alpha_memory.borrow_mut().successors.push(Rc::clone(&new));

    println!("Built {}", new.borrow());

    parent.borrow_mut().add_child(&new);

    new
}

fn join_test(tests: &[TestAtJoinNode], token: &RcCell<Token>, wme: &Wme) -> bool {
    println!(
        "Performing join tests on {tests:?} with WME {} {:?} and token {}",
        wme.id,
        wme.fields,
        token.borrow().id
    );

    for test in tests.iter() {
        let parent = Token::nth_parent(Rc::clone(token), test.distance_to_wme);

        // If the tokens are pointing to the dummy token they immediatelly get a pass
        if parent.borrow().id == 0 {
            println!("Join test successful");
            return true;
        }

        let parent = parent.borrow();
        let wme2 = &parent.wme.borrow();
        println!("Comparing WME {:?} from token {}", wme2.fields, parent.id);

        let current_value = wme[test.arg_one];
        let previous_value = wme2[test.arg_two];

        println!(
            "Testing Current WME {:?} with Previous {:?}, {current_value} != {previous_value}",
            wme.id, wme2.id,
        );

        if current_value != previous_value {
            return false;
        }
    }

    println!("Join test successful");
    true
}

fn get_join_tests_from_condition(
    condition: &Condition,
    earlier_conds: &[Condition],
) -> Vec<TestAtJoinNode> {
    let mut result = vec![];

    println!(
        "Creating join tests from {:?} and earlier {:?}",
        condition, earlier_conds
    );

    let current_condition_num = earlier_conds.len();

    for (current_idx, var) in condition.variables() {
        let Some((distance, prev_idx)) = earlier_conds
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, cond)| cond.variables().find_map(|(cond_idx, v)|
                if v == var {
                    Some((idx + 1, cond_idx))
                } else {
                    None
                }
            ))
        else {
            continue;
        };

        let test = TestAtJoinNode {
            arg_one: current_idx,
            distance_to_wme: current_condition_num - distance,
            arg_two: prev_idx,
        };

        result.push(test)
    }

    println!("Created join tests {:?}", result);
    result
}

#[cfg(test)]
mod tests {
    use crate::{id::reset, item::ConditionTest};

    use super::*;
    /*
    #[test]
    fn it_works() {
        let mut rete = Rete::new();
        let conditions = Vec::from([Condition([
            ConditionTest::Variable(1),
            ConditionTest::Constant(2),
            ConditionTest::Variable(3),
        ])]);
        rete.add_production(Production { id: 1, conditions });
        rete.add_wme([1, 2, 3]);
    }

    #[test]
    fn join_test_to_condition() {
        use ConditionTest::*;
        let condition = Condition([Variable(2), Variable(3), Variable(4)]);
        let previous = &[
            Condition([Variable(1), Variable(1), Variable(2)]),
            Condition([Variable(5), Variable(6), Variable(3)]),
        ];
        let test_nodes = get_join_tests_from_condition(&condition, previous);

        assert_eq!(
            test_nodes[0],
            TestAtJoinNode {
                arg_one: 0,
                distance_to_wme: 1,
                arg_two: 2
            }
        );

        assert_eq!(
            test_nodes[1],
            TestAtJoinNode {
                arg_one: 1,
                distance_to_wme: 0,
                arg_two: 2
            }
        );

        let c = Condition([Variable(1), Constant(0), Variable(2)]);
        let earlier = vec![
            Condition([Variable(3), Constant(1), Variable(5)]),
            Condition([Variable(1), Constant(0), Variable(7)]),
            Condition([Variable(6), Constant(0), Variable(7)]),
        ];

        let result = get_join_tests_from_condition(&c, &earlier);

        assert_eq!(
            result[0],
            TestAtJoinNode {
                arg_one: 0,
                distance_to_wme: 1,
                arg_two: 0
            }
        );

        let c = Condition([Variable(1), Constant(0), Variable(2)]);
        let earlier = vec![
            Condition([Variable(3), Constant(1), Variable(5)]),
            Condition([Variable(2), Constant(0), Variable(7)]),
            Condition([Variable(6), Constant(0), Variable(1)]),
        ];

        let result = get_join_tests_from_condition(&c, &earlier);

        assert_eq!(
            result[0],
            TestAtJoinNode {
                arg_one: 0,
                distance_to_wme: 0,
                arg_two: 2
            }
        );
        assert_eq!(
            result[1],
            TestAtJoinNode {
                arg_one: 2,
                distance_to_wme: 1,
                arg_two: 0
            }
        );
    }

    #[test]
    fn nth_parent_works() {
        let beta = BetaMemoryNode::new(None);

        let wme = Wme::new([1, 2, 3]).to_cell();

        let node = beta.to_node_cell();

        let daddy = Token::new(&node, None, &wme);

        let child_token_one = Token::new(&node, Some(Rc::downgrade(&daddy)), &wme);

        let child_token_two = Token::new(&node, Some(Rc::downgrade(&child_token_one)), &wme);

        child_token_one
            .borrow_mut()
            .children
            .push(Rc::clone(&child_token_two));
        daddy.borrow_mut().children.push(child_token_one);

        let parent = Token::nth_parent(child_token_two, 2);

        assert_eq!(parent.borrow().id, daddy.borrow().id);
        assert_eq!(
            parent.borrow().children.len(),
            daddy.borrow().children.len()
        );
    }

    #[cfg(test)]
    mod block_world {
        use crate::id::reset;

        use super::*;

        pub const B1: usize = 1;
        pub const B2: usize = 2;
        pub const B3: usize = 3;
        pub const B4: usize = 4;
        pub const B5: usize = 5;
        pub const B6: usize = 6;

        pub const ON: usize = 10;
        pub const COLOR: usize = 11;
        pub const LEFT_OF: usize = 12;

        pub const RED: usize = 20;
        pub const MAIZE: usize = 21;
        pub const BLUE: usize = 23;
        pub const TABLE: usize = 25;

        pub const W1: [usize; 3] = [B1, ON, B2];
        pub const W2: [usize; 3] = [B1, ON, B3];
        pub const W3: [usize; 3] = [B1, COLOR, RED];
        pub const W4: [usize; 3] = [B2, ON, TABLE];
        pub const W5: [usize; 3] = [B2, LEFT_OF, B3];
        pub const W6: [usize; 3] = [B2, COLOR, BLUE];
        pub const W7: [usize; 3] = [B3, LEFT_OF, B4];
        pub const W8: [usize; 3] = [B3, ON, TABLE];
        pub const W9: [usize; 3] = [B3, COLOR, RED];

        fn wmes() -> Vec<[usize; 3]> {
            vec![W1, W2, W3, W4, W5, W6, W7, W8, W9]
        }

        const V_X: ConditionTest = ConditionTest::Variable(0);
        const V_Y: ConditionTest = ConditionTest::Variable(1);
        const V_Z: ConditionTest = ConditionTest::Variable(2);
        const V_A: ConditionTest = ConditionTest::Variable(3);
        const V_B: ConditionTest = ConditionTest::Variable(4);

        const C_ON: ConditionTest = ConditionTest::Constant(ON);
        const C_LEFT_OF: ConditionTest = ConditionTest::Constant(LEFT_OF);
        const C_COLOR: ConditionTest = ConditionTest::Constant(COLOR);
        const C_RED: ConditionTest = ConditionTest::Constant(RED);
        const C_MAIZE: ConditionTest = ConditionTest::Constant(MAIZE);
        const C_BLUE: ConditionTest = ConditionTest::Constant(BLUE);

        const C1: Condition = Condition([V_X, C_ON, V_Y]);
        const C2: Condition = Condition([V_Y, C_LEFT_OF, V_Z]);
        const C3: Condition = Condition([V_Z, C_COLOR, C_RED]);
        const C4: Condition = Condition([V_A, C_COLOR, C_MAIZE]);
        const C5: Condition = Condition([V_B, C_COLOR, C_BLUE]);

        fn productions() -> Vec<Production> {
            vec![
                Production {
                    id: 9001,
                    conditions: vec![C1, C2, C3],
                },
                Production {
                    id: 9002,
                    conditions: vec![C1, C2, C4, C5],
                },
                Production {
                    id: 9003,
                    conditions: vec![C1, C2, C4, C3],
                },
            ]
        }

        #[test]
        fn add_productions() {
            let mut rete = Rete::new();
            for p in productions() {
                rete.add_production(p);
            }

            assert_eq!(rete.constant_tests.len(), 5);

            reset()
        }

        #[test]
        fn example_from_paper() {
            let mut rete = Rete::new();
            for p in productions() {
                rete.add_production(p);
            }

            for wme in wmes() {
                rete.add_wme(wme);
            }

            reset()
        }

        #[test]
        fn add_productions_then_wmes() {
            const C1: Condition = Condition([V_X, C_ON, V_Y]);
            const C2: Condition = Condition([V_Y, C_LEFT_OF, V_Z]);
            const C3: Condition = Condition([V_Z, C_COLOR, C_RED]);
            const C4: Condition = Condition([V_A, C_COLOR, C_MAIZE]);
            const C5: Condition = Condition([V_B, C_COLOR, C_BLUE]);

            let production_one = Production {
                id: 9000,
                conditions: vec![C1, C2, C3],
            };
            let production_two = Production {
                id: 9001,
                conditions: vec![C1, C2, C4, C5],
            };

            let mut rete = Rete::new();

            rete.add_production(production_one);
            rete.add_production(production_two);

            rete.add_wme([B1, ON, B2]);
            rete.add_wme([B2, LEFT_OF, B3]);
            rete.add_wme([B3, COLOR, RED]);
            rete.add_wme([B2, ON, B3]);
            rete.add_wme([B3, LEFT_OF, B4]);
            rete.add_wme([B5, COLOR, MAIZE]);
            rete.add_wme([B6, COLOR, BLUE]);

            reset()
        }

        #[test]
        fn add_wme_then_prod() {
            const C1: Condition = Condition([V_X, C_ON, V_Y]);
            const C2: Condition = Condition([V_Y, C_LEFT_OF, V_Z]);
            const C3: Condition = Condition([V_Z, C_COLOR, C_RED]);
            const C4: Condition = Condition([V_A, C_COLOR, C_MAIZE]);
            const C5: Condition = Condition([V_B, C_COLOR, C_BLUE]);

            let production_one = Production {
                id: 9000,
                conditions: vec![C1, C2, C3],
            };
            let production_two = Production {
                id: 9001,
                conditions: vec![C1, C2, C4, C5],
            };

            let mut rete = Rete::new();

            rete.add_wme([B1, ON, B2]);
            rete.add_wme([B2, LEFT_OF, B3]);
            rete.add_wme([B3, COLOR, RED]);
            rete.add_wme([B2, ON, B3]);
            rete.add_wme([B3, LEFT_OF, B4]);
            rete.add_wme([B5, COLOR, MAIZE]);
            rete.add_wme([B6, COLOR, BLUE]);

            rete.add_production(production_one);
            rete.add_production(production_two);

            rete.print_to_file("state.txt").unwrap();

            reset()
        }
    } */

    #[test]
    fn simple_wme_removal() {
        let mut rete = Rete::new();

        let id = rete.add_wme([1, 2, 3]);

        println!("ID {id}");

        rete.remove_wme(id);

        rete.print_to_file("wmetest.txt");
        assert!(rete.working_memory.is_empty());

        reset();
    }

    /*     #[test]
       fn wme_removal_with_tokens() {
           const ON: usize = 10;
           const COLOR: usize = 11;
           const LEFT_OF: usize = 12;
           const RED: usize = 20;

           const X: usize = 0;
           const Y: usize = 1;
           const Z: usize = 2;

           const V_X: ConditionTest = ConditionTest::Variable(0);
           const V_Y: ConditionTest = ConditionTest::Variable(1);
           const V_Z: ConditionTest = ConditionTest::Variable(2);

           const C_ON: ConditionTest = ConditionTest::Constant(ON);
           const C_LEFT_OF: ConditionTest = ConditionTest::Constant(LEFT_OF);
           const C_COLOR: ConditionTest = ConditionTest::Constant(COLOR);
           const C_RED: ConditionTest = ConditionTest::Constant(RED);

           const C1: Condition = Condition([V_X, C_ON, V_Y]);
           const C2: Condition = Condition([V_Y, C_LEFT_OF, V_Z]);
           const C3: Condition = Condition([V_Z, C_COLOR, C_RED]);

           const W1: [usize; 3] = [X, ON, Y];
           const W2: [usize; 3] = [Y, LEFT_OF, Z];
           const W3: [usize; 3] = [Y, COLOR, RED];

           let mut rete = Rete::new();

           let production = Production {
               id: 9000,
               conditions: vec![C1, C2, C3],
           };

           rete.add_production(production);

           let id1 = rete.add_wme(W1);
           let id2 = rete.add_wme(W2);

           rete.remove_wme(id2);

           rete.print_to_file("remove_first_state.txt").unwrap();

           rete.remove_wme(id1);

           assert!(rete.working_memory.is_empty());
           assert!(rete.dummy_top_token.borrow().children.is_empty());

           rete.print_to_file("remove_second_state.txt").unwrap();

           reset();
       }
    */
}
