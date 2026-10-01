// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/debugger/parser.c / include/mgba/internal/debugger/parser.h
//
// Notes on the port:
// - The C lexer works on `char` bytes and is replicated byte-for-byte over
//   the UTF-8 bytes of the input &str (including its quirks, e.g. the
//   LEX_EXPECT_OPERATOR2 fall-through that processes a second operator
//   character twice, and accepting ':' mid-identifier).
// - The C parser mutates a linked tree through raw parent pointers (`p`)
//   and a memcpy-based rotation. Here nodes live in a local arena (Vec)
//   with index "pointers" during parsing, then the arena tree is converted
//   to the owned `Box<ParseTree>` representation.
// - `struct ParseTree` maps to `ParseTree` with boxed optional children.

use crate::debugger::DebugConsole;
use crate::debugger::Debugger;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Operation {
    Assign,
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    And,
    Or,
    Xor,
    Less,
    Greater,
    Equal,
    NotEqual,
    LogicalAnd,
    LogicalOr,
    Le,
    Ge,
    Negate,
    Flip,
    Not,
    ShiftL,
    ShiftR,
    Dereference,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Token {
    Error,
    UInt(u32),
    Identifier(String),
    Operator(Operation),
    OpenParen,
    CloseParen,
    Segment(u32),
}

#[derive(Clone, PartialEq, Debug)]
pub struct ParseTree {
    pub token: Token,
    pub lhs: Option<Box<ParseTree>>,
    pub rhs: Option<Box<ParseTree>>,
    pub precedence: i32,
}

// enum LexState
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LexState {
    Error, // LEX_ERROR = -1
    Root,  // LEX_ROOT = 0
    ExpectIdentifier,
    ExpectBinaryFirst,
    ExpectBinary,
    ExpectDecimal,
    ExpectHexFirst,
    ExpectHex,
    ExpectPrefix,
    ExpectOperator2,
}

/// Check whether a byte may start an identifier. The C condition is
/// `tolower(token) >= 'a' && tolower(token <= 'z')`; with signed `char` and
/// ASCII this is equivalent to an ASCII alphabetic test (`[a-zA-Z]`).
fn is_identifier_start(token: u8) -> bool {
    token.is_ascii_alphabetic()
}

/// _lexOperator
fn lex_operator(lv: &mut Vec<Token>, operator: u8, state: &mut LexState) {
    if *state == LexState::ExpectOperator2 {
        let Some(lv_next) = lv.last_mut() else {
            *state = LexState::Error;
            return;
        };
        let Token::Operator(operator_value) = lv_next else {
            *lv_next = Token::Error;
            *state = LexState::Error;
            return;
        };
        match operator_value {
            Operation::And => {
                if operator == b'&' {
                    *operator_value = Operation::LogicalAnd;
                    *state = LexState::Root;
                    return;
                }
            }
            Operation::Or => {
                if operator == b'|' {
                    *operator_value = Operation::LogicalOr;
                    *state = LexState::Root;
                    return;
                }
            }
            Operation::Less => {
                if operator == b'=' {
                    *operator_value = Operation::Le;
                    *state = LexState::Root;
                    return;
                }
                if operator == b'<' {
                    *operator_value = Operation::ShiftL;
                    *state = LexState::Root;
                    return;
                }
            }
            Operation::Greater => {
                if operator == b'=' {
                    *operator_value = Operation::Ge;
                    *state = LexState::Root;
                    return;
                }
                if operator == b'>' {
                    *operator_value = Operation::ShiftR;
                    *state = LexState::Root;
                    return;
                }
            }
            Operation::Assign => {
                if operator == b'=' {
                    *operator_value = Operation::Equal;
                    *state = LexState::Root;
                    return;
                }
            }
            Operation::Not => {
                if operator == b'=' {
                    *operator_value = Operation::NotEqual;
                    *state = LexState::Root;
                    return;
                }
            }
            _ => {}
        }
        // C quirk: after marking LEX_ERROR here, execution falls through and
        // the operator is appended anyway (overwriting the error state).
        *state = LexState::Error;
    }
    let operator_value = match operator {
        b'=' => Operation::Assign,
        b'+' => Operation::Add,
        b'-' => Operation::Subtract,
        b'*' => Operation::Multiply,
        b'/' => Operation::Divide,
        b'%' => Operation::Modulo,
        b'&' => Operation::And,
        b'|' => Operation::Or,
        b'^' => Operation::Xor,
        b'<' => Operation::Less,
        b'>' => Operation::Greater,
        b'!' => Operation::Not,
        b'~' => Operation::Flip,
        _ => {
            *state = LexState::Error;
            return;
        }
    };
    *state = LexState::ExpectOperator2;
    lv.push(Token::Operator(operator_value));
}

/// _lexValue: a numeric literal is terminated by `token`
fn lex_value(lv: &mut Vec<Token>, token: u8, next: u32, state: &mut LexState) {
    match token {
        b'=' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'<' | b'>' | b'!' => {
            lv.push(Token::UInt(next));
            lex_operator(lv, token, state);
        }
        b')' => {
            lv.push(Token::UInt(next));
            lv.push(Token::CloseParen);
            *state = LexState::Root;
        }
        b' ' | b'\t' => {
            lv.push(Token::UInt(next));
            *state = LexState::Root;
        }
        _ => {
            *state = LexState::Error;
        }
    }
}

/// LEX_ROOT state (shared by the LEX_ROOT case and the LEX_EXPECT_OPERATOR2
/// fall-through). `i` is the index just past the current byte in `bytes`.
fn lex_root(
    lv: &mut Vec<Token>,
    token: u8,
    state: &mut LexState,
    next: &mut u32,
    token_start: &mut usize,
    i: usize,
) {
    *token_start = i - 1;
    match token {
        b'1'..=b'9' => {
            *state = LexState::ExpectDecimal;
            *next = (token - b'0') as u32;
        }
        b'0' => {
            *state = LexState::ExpectPrefix;
            *next = 0;
        }
        b'$' => {
            *state = LexState::ExpectHexFirst;
            *next = 0;
        }
        b'(' => {
            *state = LexState::Root;
            lv.push(Token::OpenParen);
        }
        b'=' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'<' | b'>' | b'!'
        | b'~' => {
            lex_operator(lv, token, state);
        }
        b')' => {
            // C quirk: the state is not reset here.
            lv.push(Token::CloseParen);
        }
        b' ' | b'\t' => {}
        _ => {
            if is_identifier_start(token) {
                *state = LexState::ExpectIdentifier;
            } else {
                *state = LexState::Error;
            }
        }
    }
}

/// lexExpression: tokenize `string` (up to `length`, stopping at `eol`)
/// into a token vector. Returns (tokens, number of bytes consumed).
///
/// `eol` is the set of end-of-line characters. Unlike the C version, which
/// defaults to " \r\n" when passed NULL, the Rust version always takes the
/// set explicitly; pass " \r\n" for the C NULL behavior and "" to scan to
/// the end of the string.
pub fn lex_expression(string: &str, length: usize, eol: &str) -> (Vec<Token>, usize) {
    let mut lv: Vec<Token> = Vec::new();
    if string.is_empty() || length < 1 {
        return (lv, 0);
    }

    let bytes = string.as_bytes();
    let eol = eol.as_bytes();
    let max = length.min(bytes.len());
    let mut next: u32 = 0;
    let mut adjusted: usize = 0;

    let mut state = LexState::Root;
    let mut token_start: usize = 0;

    while adjusted < max
        && bytes[adjusted] != 0
        && !eol.contains(&bytes[adjusted])
        && state != LexState::Error
    {
        let token = bytes[adjusted];
        adjusted += 1;
        match state {
            LexState::ExpectOperator2 => {
                match token {
                    b'&' | b'|' | b'=' | b'<' | b'>' => {
                        lex_operator(&mut lv, token, &mut state)
                    }
                    _ => {}
                }
                if state != LexState::ExpectOperator2 {
                    // Done with the two-char operator scan.
                } else {
                    // Fall through to LEX_ROOT.
                    lex_root(&mut lv, token, &mut state, &mut next, &mut token_start, adjusted);
                }
            }
            LexState::Root => {
                lex_root(&mut lv, token, &mut state, &mut next, &mut token_start, adjusted);
            }
            LexState::ExpectIdentifier => match token {
                b'=' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'<' | b'>'
                | b'!' | b'~' => {
                    lv.push(Token::Identifier(
                        String::from_utf8_lossy(&bytes[token_start..adjusted - 1]).into_owned(),
                    ));
                    lex_operator(&mut lv, token, &mut state);
                }
                b')' => {
                    lv.push(Token::Identifier(
                        String::from_utf8_lossy(&bytes[token_start..adjusted - 1]).into_owned(),
                    ));
                    lv.push(Token::CloseParen);
                    state = LexState::Root;
                }
                b' ' | b'\t' => {
                    lv.push(Token::Identifier(
                        String::from_utf8_lossy(&bytes[token_start..adjusted - 1]).into_owned(),
                    ));
                    state = LexState::Root;
                }
                _ => {}
            },
            LexState::ExpectBinaryFirst | LexState::ExpectBinary => {
                if state == LexState::ExpectBinaryFirst {
                    state = LexState::ExpectBinary;
                }
                match token {
                    b'0' | b'1' => {
                        // TODO: handle overflow (C wraps a uint32_t)
                        next = next.wrapping_mul(2).wrapping_add((token - b'0') as u32);
                    }
                    _ => {
                        lex_value(&mut lv, token, next, &mut state);
                    }
                }
            }
            LexState::ExpectDecimal => match token {
                b'0'..=b'9' => {
                    // TODO: handle overflow (C wraps a uint32_t)
                    next = next.wrapping_mul(10).wrapping_add((token - b'0') as u32);
                }
                _ => {
                    lex_value(&mut lv, token, next, &mut state);
                }
            },
            LexState::ExpectHexFirst | LexState::ExpectHex => {
                if state == LexState::ExpectHexFirst {
                    state = LexState::ExpectHex;
                }
                match token {
                    b'0'..=b'9' => {
                        // TODO: handle overflow (C wraps a uint32_t)
                        next = next.wrapping_mul(16).wrapping_add((token - b'0') as u32);
                    }
                    b'A'..=b'F' => {
                        next = next.wrapping_mul(16).wrapping_add((token - b'A') as u32 + 10);
                    }
                    b'a'..=b'f' => {
                        next = next.wrapping_mul(16).wrapping_add((token - b'a') as u32 + 10);
                    }
                    b':' => {
                        lv.push(Token::Segment(next));
                        next = 0;
                    }
                    _ => {
                        lex_value(&mut lv, token, next, &mut state);
                    }
                }
            }
            LexState::ExpectPrefix => match token {
                b'X' | b'x' => {
                    next = 0;
                    state = LexState::ExpectHexFirst;
                }
                b'B' | b'b' => {
                    next = 0;
                    state = LexState::ExpectBinaryFirst;
                }
                b'0'..=b'9' => {
                    next = (token - b'0') as u32;
                    state = LexState::ExpectDecimal;
                }
                _ => {
                    lex_value(&mut lv, token, next, &mut state);
                }
            },
            LexState::Error => {
                // This shouldn't be reached
            }
        }
    }

    match state {
        LexState::ExpectBinary | LexState::ExpectDecimal | LexState::ExpectHex
        | LexState::ExpectPrefix => {
            lv.push(Token::UInt(next));
        }
        LexState::ExpectIdentifier => {
            lv.push(Token::Identifier(
                String::from_utf8_lossy(&bytes[token_start..adjusted]).into_owned(),
            ));
        }
        LexState::Root | LexState::ExpectOperator2 => {}
        LexState::ExpectBinaryFirst | LexState::ExpectHexFirst | LexState::Error => {
            lv.push(Token::Error);
        }
    }
    (lv, adjusted)
}

/// _operatorPrecedence
fn operator_precedence(operation: Operation) -> i32 {
    match operation {
        Operation::Assign => 14,
        Operation::Add | Operation::Subtract => 4,
        Operation::Multiply | Operation::Divide | Operation::Modulo => 3,
        Operation::And => 8,
        Operation::Or => 10,
        Operation::Xor => 9,
        Operation::Less | Operation::Greater => 6,
        Operation::Equal | Operation::NotEqual => 7,
        Operation::Le | Operation::Ge => 6,
        Operation::LogicalAnd => 11,
        Operation::LogicalOr => 12,
        Operation::Negate | Operation::Flip | Operation::Not => 2,
        Operation::ShiftL | Operation::ShiftR => 5,
        Operation::Dereference => 2,
    }
}

/// Arena node standing in for `struct ParseTree` while parsing; the C `p`
/// parent pointer becomes an index, as do `lhs`/`rhs`.
struct ParseNode {
    token: Token,
    p: Option<usize>,
    lhs: Option<usize>,
    rhs: Option<usize>,
    precedence: i32,
}

/// parseTreeCreate
fn parse_tree_create(arena: &mut Vec<ParseNode>) -> usize {
    arena.push(ParseNode {
        token: Token::Error,
        p: None,
        lhs: None,
        rhs: None,
        precedence: i32::MAX,
    });
    arena.len() - 1
}

/// _parseExpression
fn parse_expression(
    arena: &mut Vec<ParseNode>,
    root: usize,
    lv: &mut Vec<Token>,
    open_parens: &mut i32,
) -> usize {
    let mut pop = false;
    let mut precedence = i32::MAX;
    let mut tree: Option<usize> = Some(root);
    let mut i: usize = 0;
    while i < lv.len() {
        let t = tree.expect("tree is Some while parsing");
        match &lv[i] {
            Token::Identifier(_) | Token::UInt(_) => {
                if arena[t].token == Token::Error {
                    arena[t].token = lv[i].clone();
                    i += 1;
                } else {
                    arena[t].token = Token::Error;
                    i += 1;
                    pop = true;
                }
            }
            Token::Segment(value) => {
                let value = *value;
                let lhs = parse_tree_create(arena);
                arena[lhs].token = Token::UInt(value);
                arena[lhs].p = Some(t);
                arena[lhs].precedence = precedence;
                arena[t].lhs = Some(lhs);
                let rhs = parse_tree_create(arena);
                arena[rhs].p = Some(t);
                arena[rhs].precedence = precedence;
                arena[t].rhs = Some(rhs);
                arena[t].token = Token::Segment(value);
                tree = Some(rhs);
                i += 1;
            }
            Token::OpenParen => {
                *open_parens += 1;
                precedence = i32::MAX;
                i += 1;
            }
            Token::CloseParen => {
                if *open_parens <= 0 {
                    arena[t].token = Token::Error;
                }
                *open_parens -= 1;
                i += 1;
                pop = true;
            }
            Token::Operator(_) => {
                if arena[t].token == Token::Error {
                    match lv[i] {
                        Token::Operator(Operation::Subtract) => {
                            lv[i] = Token::Operator(Operation::Negate);
                        }
                        Token::Operator(Operation::Multiply) => {
                            lv[i] = Token::Operator(Operation::Dereference);
                        }
                        _ => {}
                    }
                }
                let Token::Operator(operation) = lv[i] else {
                    unreachable!()
                };
                let new_precedence = operator_precedence(operation);
                if new_precedence < precedence {
                    // Rotate: copy the current node into a new lhs child and
                    // turn the current node into the operator node.
                    let new_tree = parse_tree_create(arena);
                    arena[new_tree].token = arena[t].token.clone();
                    arena[new_tree].lhs = arena[t].lhs;
                    arena[new_tree].rhs = arena[t].rhs;
                    arena[new_tree].precedence = arena[t].precedence;
                    if let Some(lhs) = arena[new_tree].lhs {
                        arena[lhs].p = Some(new_tree);
                    }
                    if let Some(rhs) = arena[new_tree].rhs {
                        arena[rhs].p = Some(new_tree);
                    }
                    arena[new_tree].p = Some(t);
                    arena[t].lhs = Some(new_tree);
                    let rhs = parse_tree_create(arena);
                    arena[rhs].p = Some(t);
                    arena[rhs].precedence = new_precedence;
                    arena[t].rhs = Some(rhs);
                    precedence = new_precedence;
                    arena[t].token = lv[i].clone();
                    tree = Some(rhs);
                    i += 1;
                } else {
                    pop = true;
                }
            }
            Token::Error => {
                arena[t].token = Token::Error;
                i += 1;
                pop = true;
            }
        }

        if pop {
            if arena[t].token == Token::Error {
                if let Some(p) = arena[t].p {
                    arena[p].token = Token::Error;
                }
            }
            tree = arena[t].p;
            pop = false;
            match tree {
                None => break,
                Some(node) => precedence = arena[node].precedence,
            }
        }
    }

    i
}

/// Convert the arena representation to the boxed `ParseTree`.
fn arena_to_tree(arena: &[ParseNode], idx: usize) -> Box<ParseTree> {
    Box::new(ParseTree {
        token: arena[idx].token.clone(),
        lhs: arena[idx].lhs.map(|lhs| arena_to_tree(arena, lhs)),
        rhs: arena[idx].rhs.map(|rhs| arena_to_tree(arena, rhs)),
        precedence: arena[idx].precedence,
    })
}

/// parseLexedExpression (returns None on parse error)
pub fn parse_lexed_expression(tokens: &[Token]) -> Option<Box<ParseTree>> {
    let mut arena: Vec<ParseNode> = Vec::new();
    let root = parse_tree_create(&mut arena);
    // The parser mutates operator tokens in place (e.g. OP_SUBTRACT becomes
    // OP_NEGATE in operand position), so work on a copy.
    let mut lv = tokens.to_vec();

    let mut open_parens: i32 = 0;
    parse_expression(&mut arena, root, &mut lv, &mut open_parens);
    if open_parens != 0 {
        arena[root].token = Token::Error;
    }
    if arena[root].token == Token::Error {
        None
    } else {
        Some(arena_to_tree(&arena, root))
    }
}

/// _performOperation: apply `operation` to `current`/`next`, returning the
/// new (value, segment).
fn perform_operation(
    console: &mut dyn DebugConsole,
    operation: Operation,
    current: i32,
    next: i32,
    segment: i32,
) -> Option<(i32, i32)> {
    let value = match operation {
        Operation::Assign => next,
        Operation::Add => current.wrapping_add(next),
        Operation::Subtract => current.wrapping_sub(next),
        Operation::Multiply => current.wrapping_mul(next),
        Operation::Divide => {
            if next != 0 {
                current.wrapping_div(next)
            } else {
                return None;
            }
        }
        Operation::Modulo => {
            if next != 0 {
                current.wrapping_rem(next)
            } else {
                return None;
            }
        }
        Operation::And => current & next,
        Operation::Or => current | next,
        Operation::Xor => current ^ next,
        Operation::Less => (current < next) as i32,
        Operation::Greater => (current > next) as i32,
        Operation::Equal => (current == next) as i32,
        Operation::NotEqual => (current != next) as i32,
        Operation::LogicalAnd => (current != 0 && next != 0) as i32,
        Operation::LogicalOr => (current != 0 || next != 0) as i32,
        Operation::Le => (current <= next) as i32,
        Operation::Ge => (current >= next) as i32,
        Operation::Negate => next.wrapping_neg(),
        Operation::Flip => !next,
        Operation::Not => (next == 0) as i32,
        // C shift-by->=32 is UB; in practice x86 masks the count, which is
        // what wrapping_shl/wrapping_shr do.
        Operation::ShiftL => current.wrapping_shl(next as u32),
        Operation::ShiftR => current.wrapping_shr(next as u32),
        Operation::Dereference => {
            // C: busRead8 when segment < 0, rawRead8 otherwise. The Rust
            // DebugConsole only exposes the raw (no-side-effect) accessor,
            // so it is used for both, at width 1 as in the C.
            let value = console.dbg_raw_read(next as u32, segment, 1);
            return Some((value as i32, -1));
        }
    };
    Some((value, segment))
}

/// Whether the operator takes two operands in the C evaluator
/// (`nextBranch = 0` in mDebuggerEvaluateParseTree).
fn is_binary(operation: Operation) -> bool {
    match operation {
        Operation::Assign
        | Operation::Add
        | Operation::Subtract
        | Operation::Multiply
        | Operation::Divide
        | Operation::Modulo
        | Operation::And
        | Operation::Or
        | Operation::Xor
        | Operation::Less
        | Operation::Greater
        | Operation::Equal
        | Operation::NotEqual
        | Operation::LogicalAnd
        | Operation::LogicalOr
        | Operation::Le
        | Operation::Ge
        | Operation::ShiftL
        | Operation::ShiftR => true,
        Operation::Negate | Operation::Flip | Operation::Not | Operation::Dereference => false,
    }
}

/// Recursive equivalent of mDebuggerEvaluateParseTree's iterative walk.
/// `cur` is the (value, segment) accumulator the C keeps in
/// tmpVal/tmpSegment; the C stack-based traversal is equivalent to threading
/// it through the recursion as done here.
fn evaluate(
    debugger: &mut Debugger,
    console: &mut dyn DebugConsole,
    tree: &ParseTree,
    cur: (i32, i32),
) -> Option<(i32, i32)> {
    match &tree.token {
        Token::UInt(value) => Some((*value as i32, -1)),
        Token::Identifier(name) => debugger.lookup_identifier(console, name),
        Token::Segment(_) => {
            let lhs = tree.lhs.as_ref()?;
            let rhs = tree.rhs.as_ref()?;
            let l = evaluate(debugger, console, lhs, cur)?;
            let r = evaluate(debugger, console, rhs, l)?;
            // The segment number is the lhs value; the segment the rhs
            // produced is discarded.
            Some((r.0, l.0))
        }
        Token::Operator(operation) => {
            let operation = *operation;
            if is_binary(operation) {
                let lhs = tree.lhs.as_ref()?;
                let rhs = tree.rhs.as_ref()?;
                if operation == Operation::Assign {
                    return evaluate_assign(debugger, console, lhs, rhs, cur);
                }
                let l = evaluate(debugger, console, lhs, cur)?;
                let r = evaluate(debugger, console, rhs, l)?;
                perform_operation(console, operation, l.0, r.0, l.1)
            } else {
                // Unary operator; the operand lives in rhs (C: nextBranch=1).
                let rhs = tree.rhs.as_ref()?;
                let r = evaluate(debugger, console, rhs, cur)?;
                perform_operation(console, operation, cur.0, r.0, cur.1)
            }
        }
        Token::Error | Token::OpenParen | Token::CloseParen => None,
    }
}

/// OP_ASSIGN evaluation. The vendored C treats assignment as a plain binary
/// operation (`current = next`); the actual register/memory writes are done
/// here instead, through the DebugConsole.
fn evaluate_assign(
    debugger: &mut Debugger,
    console: &mut dyn DebugConsole,
    lhs: &ParseTree,
    rhs: &ParseTree,
    cur: (i32, i32),
) -> Option<(i32, i32)> {
    match &lhs.token {
        Token::Identifier(name) => {
            let r = evaluate(debugger, console, rhs, cur)?;
            if !console.dbg_write_register(name, r.0) {
                return None;
            }
            Some((r.0, -1))
        }
        Token::Operator(Operation::Dereference) => {
            // `*addr = value`: write a byte (width 1, like the C deref read)
            // through rawWrite. `addr` may itself carry a segment via `seg:`.
            let address_node = lhs.rhs.as_ref()?;
            let r = evaluate(debugger, console, rhs, cur)?;
            let address = evaluate(debugger, console, address_node, r)?;
            console.dbg_raw_write(address.0 as u32, address.1, 1, r.0 as u32);
            Some((r.0, -1))
        }
        // Anything else cannot be assigned to; behave like the C (plain
        // binary evaluation, result is the rhs value).
        _ => {
            let l = evaluate(debugger, console, lhs, cur)?;
            let r = evaluate(debugger, console, rhs, l)?;
            perform_operation(console, Operation::Assign, l.0, r.0, l.1)
        }
    }
}

/// mDebuggerEvaluateParseTree: evaluate the expression, returning
/// (value, segment); the segment is -1 unless set by a `seg:` token.
/// `OP_ASSIGN` writes through registers/memory.
pub fn evaluate_parse_tree(
    debugger: &mut Debugger,
    console: &mut dyn DebugConsole,
    tree: &ParseTree,
) -> Option<(i32, i32)> {
    evaluate(debugger, console, tree, (0, -1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debugger::{Breakpoint, Debugger, Watchpoint};

    /// Minimal DebugConsole for evaluation tests: one register "r0" = 42,
    /// all memory reads return 0.
    struct MockConsole {
        r0: i32,
    }

    impl DebugConsole for MockConsole {
        fn dbg_run_loop(&mut self) {}
        fn dbg_step(&mut self) {}
        fn dbg_frame_counter(&self) -> u32 {
            0
        }
        fn dbg_read_register(&self, name: &str) -> Option<i32> {
            match name {
                "r0" => Some(self.r0),
                _ => None,
            }
        }
        fn dbg_write_register(&mut self, name: &str, value: i32) -> bool {
            if name == "r0" {
                self.r0 = value;
                true
            } else {
                false
            }
        }
        fn dbg_raw_read(&mut self, _address: u32, _segment: i32, _width: u32) -> u32 {
            0
        }
        fn dbg_raw_write(&mut self, _address: u32, _segment: i32, _width: u32, _value: u32) {}
        fn dbg_lookup_identifier(&mut self, _name: &str) -> Option<(i32, i32)> {
            None
        }
        fn dbg_set_breakpoint(&mut self, _owner: Option<usize>, _bp: &Breakpoint) -> i64 {
            -1
        }
        fn dbg_list_breakpoints(&self, _owner: Option<usize>) -> Vec<Breakpoint> {
            Vec::new()
        }
        fn dbg_clear_breakpoint(&mut self, _id: i64) -> bool {
            false
        }
        fn dbg_toggle_breakpoint(&mut self, _id: i64, _status: bool) -> bool {
            false
        }
        fn dbg_set_watchpoint(&mut self, _owner: Option<usize>, _wp: &Watchpoint) -> i64 {
            -1
        }
        fn dbg_list_watchpoints(&self, _owner: Option<usize>) -> Vec<Watchpoint> {
            Vec::new()
        }
        fn dbg_trace(&mut self) -> String {
            String::new()
        }
            }

    fn lex(s: &str) -> (Vec<Token>, usize) {
        lex_expression(s, s.len(), "")
    }

    fn eval(s: &str, console: &mut MockConsole) -> Option<(i32, i32)> {
        let mut debugger = Debugger::new();
        let (tokens, _) = lex(s);
        let tree = parse_lexed_expression(&tokens)?;
        evaluate_parse_tree(&mut debugger, console, &tree)
    }

    #[test]
    fn lex_simple_expression() {
        let (tokens, adjusted) = lex("1+2*3");
        assert_eq!(adjusted, 5);
        assert_eq!(
            tokens,
            vec![
                Token::UInt(1),
                Token::Operator(Operation::Add),
                Token::UInt(2),
                Token::Operator(Operation::Multiply),
                Token::UInt(3),
            ]
        );
    }

    #[test]
    fn lex_hex_forms() {
        let (tokens, adjusted) = lex("0x10 - $20");
        assert_eq!(adjusted, 10);
        assert_eq!(
            tokens,
            vec![
                Token::UInt(0x10),
                Token::Operator(Operation::Subtract),
                Token::UInt(0x20),
            ]
        );
    }

    #[test]
    fn lex_identifier() {
        let (tokens, adjusted) = lex("abc");
        assert_eq!(adjusted, 3);
        assert_eq!(tokens, vec![Token::Identifier("abc".to_string())]);
    }

    #[test]
    fn lex_segment() {
        let (tokens, adjusted) = lex("$01:0010");
        assert_eq!(adjusted, 8);
        assert_eq!(tokens, vec![Token::Segment(1), Token::UInt(0x10)]);
    }

    #[test]
    fn parse_error_cases() {
        assert!(parse_lexed_expression(&lex("1 2").0).is_none());
        assert!(parse_lexed_expression(&lex("").0).is_none());
        assert!(parse_lexed_expression(&lex("(1").0).is_none());
    }

    #[test]
    fn eval_precedence() {
        let mut console = MockConsole { r0: 42 };
        assert_eq!(eval("1+2*3", &mut console), Some((7, -1)));
    }

    #[test]
    fn eval_parens() {
        let mut console = MockConsole { r0: 42 };
        assert_eq!(eval("(1+2)*3", &mut console), Some((9, -1)));
    }

    #[test]
    fn eval_shift() {
        let mut console = MockConsole { r0: 42 };
        assert_eq!(eval("2<<3", &mut console), Some((16, -1)));
    }

    #[test]
    fn eval_identifier() {
        let mut console = MockConsole { r0: 42 };
        assert_eq!(eval("r0", &mut console), Some((42, -1)));
        assert_eq!(eval("r0+8", &mut console), Some((50, -1)));
        assert_eq!(eval("r1", &mut console), None);
    }

    #[test]
    fn eval_assign_register() {
        let mut console = MockConsole { r0: 42 };
        assert_eq!(eval("r0 = 7", &mut console), Some((7, -1)));
        assert_eq!(console.r0, 7);
    }

    #[test]
    fn eval_divide_by_zero() {
        let mut console = MockConsole { r0: 42 };
        assert_eq!(eval("1/0", &mut console), None);
        assert_eq!(eval("1%0", &mut console), None);
    }

    #[test]
    fn eval_segment_value() {
        let mut console = MockConsole { r0: 42 };
        assert_eq!(eval("$4:10", &mut console), Some((0x10, 4)));
    }

    // Parity checks for quirks of the C implementation, so they are not
    // "fixed" by accident. See mgba/src/debugger/parser.c and its tests.
    #[test]
    fn c_quirk_parity() {
        // C lexes "1&|" as [UInt(1), And, LogicalOr] due to the
        // LEX_EXPECT_OPERATOR2 fall-through.
        assert_eq!(
            lex("1&|").0,
            vec![
                Token::UInt(1),
                Token::Operator(Operation::And),
                Token::Operator(Operation::LogicalOr),
            ]
        );
        // C parses "+" successfully (root OP_ADD with two ERROR children),
        // but evaluating it fails.
        let (tokens, _) = lex("+");
        let tree = parse_lexed_expression(&tokens).expect("parse should succeed");
        assert_eq!(tree.token, Token::Operator(Operation::Add));
        assert_eq!(tree.lhs.as_ref().unwrap().token, Token::Error);
        assert_eq!(tree.rhs.as_ref().unwrap().token, Token::Error);
        let mut console = MockConsole { r0: 42 };
        let mut debugger = Debugger::new();
        assert_eq!(evaluate_parse_tree(&mut debugger, &mut console, &tree), None);
        // "1 2" and "(1" fail to parse; "1+*2" parses (lhs ERR hole)
        // and evaluates to 1 + rawRead8(2).
        let (tokens, _) = lex("1+*2");
        let tree = parse_lexed_expression(&tokens).expect("parse should succeed");
        assert_eq!(tree.token, Token::Operator(Operation::Add));
        assert_eq!(
            tree.rhs.as_ref().unwrap().token,
            Token::Operator(Operation::Dereference)
        );
        let mut debugger = Debugger::new();
        assert_eq!(evaluate_parse_tree(&mut debugger, &mut console, &tree), Some((1, -1)));
        // Left associativity and precedence climbing.
        assert_eq!(eval("10-4-3", &mut console), Some((3, -1)));
        assert_eq!(eval("2*3%4", &mut console), Some((2, -1)));
        assert_eq!(eval("1|2&3", &mut console), Some((3, -1)));
        assert_eq!(eval("~0", &mut console), Some((-1, -1)));
        assert_eq!(eval("-5+5", &mut console), Some((0, -1)));
        assert_eq!(eval("!1", &mut console), Some((0, -1)));
        assert_eq!(eval("1<2", &mut console), Some((1, -1)));
        assert_eq!(eval("2==2", &mut console), Some((1, -1)));
        assert_eq!(eval("1&&0", &mut console), Some((0, -1)));
        assert_eq!(eval("0||1", &mut console), Some((1, -1)));
        // Truncated numbers lex to Error (C lexer tests).
        assert_eq!(lex("0x").0, vec![Token::Error]);
        assert_eq!(lex("0b").0, vec![Token::Error]);
        assert_eq!(lex("$").0, vec![Token::Error]);
        assert_eq!(lex("1a").0, vec![Token::Error]);
        assert_eq!(lex("0b12").0, vec![Token::Error]);
        // Binary / identifiers / parens lexing
        assert_eq!(lex("0b101").0, vec![Token::UInt(5)]);
        assert_eq!(lex(" ( 1 + 2 ) ").0.len(), 5);
        assert_eq!(lex("x!=").0.len(), 2);
        assert_eq!(lex("!!1").0.len(), 3);
    }
}
