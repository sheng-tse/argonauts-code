//! The tree-walking evaluator: statements, expressions, scopes, and calls.

use std::collections::HashMap;
use std::rc::Rc;

use starlark_syntax::codemap::Span;
use starlark_syntax::lexer::TokenInt;
use starlark_syntax::syntax::ast::{
    ArgumentP, AssignOp, AssignTargetP, AstArgument, AstAssignTarget, AstExpr, AstLiteral,
    AstNoPayload, AstParameter, AstStmt, BinOp, Clause, ClauseP, DefP, ExprP, ForClause, ForP,
    LambdaP, ParameterP, StmtP,
};

use super::builtins;
use super::value::{
    Body, Builtin, Dict, Function, Param, Value, binary, check_len, collect, items,
};
use super::{Eval, MAX_DEPTH, STEP_LIMIT, fail};

/// A call's arguments, positional ones first.
pub struct Args {
    pub positional: Vec<Value>,
    pub named: Vec<(String, Value)>,
}

enum Flow {
    Next,
    Break,
    Continue,
    Return(Value),
}

#[derive(Default)]
struct Frame {
    locals: HashMap<String, Value>,
    /// Comprehension variables, innermost last. They never leak into the enclosing scope.
    scopes: Vec<HashMap<String, Value>>,
    captured: Rc<HashMap<String, Value>>,
}

pub struct Interp {
    globals: HashMap<String, Value>,
    /// The top level's own comprehension scopes; its variables are the globals.
    module: Frame,
    frames: Vec<Frame>,
    calls: Vec<usize>,
    steps: u64,
    next_id: usize,
    workflow: Option<Value>,
}

impl Interp {
    pub fn new() -> Self {
        Self {
            globals: HashMap::new(),
            module: Frame::default(),
            frames: Vec::new(),
            calls: Vec::new(),
            steps: 0,
            next_id: 0,
            workflow: None,
        }
    }

    pub fn run(&mut self, statement: &AstStmt) -> Eval<()> {
        match self.exec(statement)? {
            Flow::Next => Ok(()),
            Flow::Return(_) => fail(statement.span, "`return` outside a function"),
            Flow::Break | Flow::Continue => {
                fail(statement.span, "`break` or `continue` outside a loop")
            }
        }
    }

    pub fn into_workflow(self) -> Option<Value> {
        self.workflow
    }

    pub fn set_workflow(&mut self, workflow: Value, span: Span) -> Eval<()> {
        if self.workflow.is_some() {
            return fail(span, "a script calls workflow(...) once");
        }
        self.workflow = Some(workflow);
        Ok(())
    }

    pub fn tick(&mut self, span: Span) -> Eval<()> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            fail(
                span,
                format!(
                    "the script ran past its step limit of {STEP_LIMIT}; building a workflow \
                     never needs that many"
                ),
            )
        } else {
            Ok(())
        }
    }

    fn exec(&mut self, statement: &AstStmt) -> Eval<Flow> {
        self.tick(statement.span)?;
        match &statement.node {
            StmtP::Break => Ok(Flow::Break),
            StmtP::Continue => Ok(Flow::Continue),
            StmtP::Pass => Ok(Flow::Next),
            StmtP::Return(value) => Ok(Flow::Return(match value {
                Some(expr) => self.eval(expr)?,
                None => Value::None,
            })),
            StmtP::Expression(expr) => {
                self.eval(expr)?;
                Ok(Flow::Next)
            }
            StmtP::Assign(assign) => {
                let value = self.eval(&assign.rhs)?;
                self.assign(&assign.lhs, value, false)?;
                Ok(Flow::Next)
            }
            StmtP::AssignModify(target, op, rhs) => {
                self.assign_modify(target, *op, rhs)?;
                Ok(Flow::Next)
            }
            StmtP::Statements(statements) => {
                for statement in statements {
                    match self.exec(statement)? {
                        Flow::Next => {}
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Next)
            }
            StmtP::If(condition, body) => {
                if self.eval(condition)?.truthy() {
                    self.exec(body)
                } else {
                    Ok(Flow::Next)
                }
            }
            StmtP::IfElse(condition, branches) => {
                let (yes, no) = &**branches;
                if self.eval(condition)?.truthy() {
                    self.exec(yes)
                } else {
                    self.exec(no)
                }
            }
            StmtP::For(each) => self.exec_for(each),
            StmtP::Def(def) => {
                self.define(def)?;
                Ok(Flow::Next)
            }
            StmtP::Load(_) => fail(
                statement.span,
                "`load` isn't available: a workflow builder has only its own file",
            ),
        }
    }

    fn exec_for(&mut self, each: &ForP<AstNoPayload>) -> Eval<Flow> {
        let over = self.eval(&each.over)?;
        for item in items(&over, each.over.span)? {
            self.tick(each.over.span)?;
            self.assign(&each.var, item, false)?;
            match self.exec(&each.body)? {
                Flow::Break => break,
                Flow::Next | Flow::Continue => {}
                done @ Flow::Return(_) => return Ok(done),
            }
        }
        Ok(Flow::Next)
    }

    fn define(&mut self, def: &DefP<AstNoPayload>) -> Eval<()> {
        let params = self.parameters(&def.params)?;
        let function = Function {
            name: def.name.node.ident.clone(),
            params,
            body: Body::Block((*def.body).clone()),
            captured: self.capture(),
            id: self.fresh_id(),
        };
        self.bind(
            &def.name.node.ident,
            Value::Function(Rc::new(function)),
            false,
        );
        Ok(())
    }

    fn lambda(&mut self, lambda: &LambdaP<AstNoPayload>) -> Eval<Value> {
        let params = self.parameters(&lambda.params)?;
        Ok(Value::Function(Rc::new(Function {
            name: "lambda".to_owned(),
            params,
            body: Body::Lambda((*lambda.body).clone()),
            captured: self.capture(),
            id: self.fresh_id(),
        })))
    }

    fn fresh_id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }

    fn parameters(&mut self, params: &[AstParameter]) -> Eval<Vec<Param>> {
        let mut out = Vec::new();
        for param in params {
            match &param.node {
                ParameterP::Normal(name, _, Some(default)) => {
                    let value = self.eval(default)?;
                    out.push(Param::Optional(name.node.ident.clone(), value));
                }
                ParameterP::Normal(name, _, None) => {
                    out.push(Param::Required(name.node.ident.clone()))
                }
                ParameterP::Args(name, _) => out.push(Param::Args(name.node.ident.clone())),
                ParameterP::KwArgs(name, _) => out.push(Param::Kwargs(name.node.ident.clone())),
                ParameterP::NoArgs => out.push(Param::KeywordOnly),
                ParameterP::Slash => {}
            }
        }
        Ok(out)
    }

    /// What a function defined here can see of its enclosing function. At the top level that's
    /// nothing, since globals are read when the function runs.
    fn capture(&self) -> Rc<HashMap<String, Value>> {
        let Some(frame) = self.frames.last() else {
            return Rc::new(HashMap::new());
        };
        let mut seen = (*frame.captured).clone();
        seen.extend(frame.locals.iter().map(|(k, v)| (k.clone(), v.clone())));
        for scope in &frame.scopes {
            seen.extend(scope.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        Rc::new(seen)
    }

    fn frame(&self) -> &Frame {
        self.frames.last().unwrap_or(&self.module)
    }

    fn frame_mut(&mut self) -> &mut Frame {
        match self.frames.last_mut() {
            Some(frame) => frame,
            None => &mut self.module,
        }
    }

    fn lookup(&self, name: &str, span: Span) -> Eval<Value> {
        let frame = self.frame();
        for scope in frame.scopes.iter().rev() {
            if let Some(value) = scope.get(name) {
                return Ok(value.clone());
            }
        }
        if let Some(value) = frame
            .locals
            .get(name)
            .or_else(|| frame.captured.get(name))
            .or_else(|| self.globals.get(name))
        {
            return Ok(value.clone());
        }
        // The parser reads these as names, as Starlark's own globals.
        match name {
            "True" => return Ok(Value::Bool(true)),
            "False" => return Ok(Value::Bool(false)),
            "None" => return Ok(Value::None),
            _ => {}
        }
        if let Some(builtin) = Builtin::named(name) {
            return Ok(Value::Builtin(builtin));
        }
        fail(
            span,
            format!(
                "`{name}` isn't defined; a workflow builder has the step builders, phase(...), \
                 workflow(...), and Starlark's basics, with no file, network, clock, or randomness"
            ),
        )
    }

    fn bind(&mut self, name: &str, value: Value, comprehension: bool) {
        if comprehension && let Some(scope) = self.frame_mut().scopes.last_mut() {
            scope.insert(name.to_owned(), value);
            return;
        }
        match self.frames.last_mut() {
            Some(frame) => {
                frame.locals.insert(name.to_owned(), value);
            }
            None => {
                self.globals.insert(name.to_owned(), value);
            }
        }
    }

    fn assign(&mut self, target: &AstAssignTarget, value: Value, comprehension: bool) -> Eval<()> {
        match &target.node {
            AssignTargetP::Identifier(ident) => {
                self.bind(&ident.node.ident, value, comprehension);
                Ok(())
            }
            AssignTargetP::Tuple(targets) => {
                let values = collect(&value, target.span)?;
                if values.len() != targets.len() {
                    return fail(
                        target.span,
                        format!(
                            "can't unpack {} values into {} names",
                            values.len(),
                            targets.len()
                        ),
                    );
                }
                for (target, value) in targets.iter().zip(values) {
                    self.assign(target, value, comprehension)?;
                }
                Ok(())
            }
            AssignTargetP::Index(pair) => {
                let (container, index) = &**pair;
                let container = self.eval(container)?;
                let index = self.eval(index)?;
                builtins::set_index(&container, index, value, target.span)
            }
            AssignTargetP::Dot(..) => fail(target.span, "attributes can't be assigned"),
        }
    }

    fn assign_modify(&mut self, target: &AstAssignTarget, op: AssignOp, rhs: &AstExpr) -> Eval<()> {
        let current = match &target.node {
            AssignTargetP::Identifier(ident) => self.lookup(&ident.node.ident, target.span)?,
            AssignTargetP::Index(pair) => {
                let (container, index) = &**pair;
                let container = self.eval(container)?;
                let index = self.eval(index)?;
                builtins::index(&container, &index, target.span)?
            }
            _ => return fail(target.span, "that can't be updated in place"),
        };
        let rhs = self.eval(rhs)?;
        // `+=` grows a list in place, as Starlark's does, so aliases see the new items.
        if let (AssignOp::Add, Value::List(list)) = (op, &current) {
            let more = collect(&rhs, target.span)?;
            builtins::check_store(&current, &more, target.span)?;
            let mut list = list.borrow_mut();
            list.extend(more);
            return check_len(list.len(), target.span);
        }
        let op = match op {
            AssignOp::Add => BinOp::Add,
            AssignOp::Subtract => BinOp::Subtract,
            AssignOp::Multiply => BinOp::Multiply,
            AssignOp::Divide => BinOp::Divide,
            AssignOp::FloorDivide => BinOp::FloorDivide,
            AssignOp::Percent => BinOp::Percent,
            AssignOp::BitAnd => BinOp::BitAnd,
            AssignOp::BitOr => BinOp::BitOr,
            AssignOp::BitXor => BinOp::BitXor,
            AssignOp::LeftShift => BinOp::LeftShift,
            AssignOp::RightShift => BinOp::RightShift,
        };
        let updated = binary(op, current, rhs, target.span)?;
        self.assign(target, updated, false)
    }

    pub fn eval(&mut self, expr: &AstExpr) -> Eval<Value> {
        self.tick(expr.span)?;
        let span = expr.span;
        match &expr.node {
            ExprP::Identifier(ident) => self.lookup(&ident.node.ident, span),
            ExprP::Literal(literal) => literal_value(literal, span),
            ExprP::Tuple(items) => Ok(Value::tuple(self.eval_all(items)?)),
            ExprP::List(items) => {
                let items = self.eval_all(items)?;
                check_len(items.len(), span)?;
                Ok(Value::list(items))
            }
            ExprP::Dict(pairs) => {
                let mut dict = Dict::default();
                for (key, value) in pairs {
                    let key = self.eval(key)?;
                    if !key.hashable() {
                        return fail(span, format!("a {} can't be a dict key", key.type_name()));
                    }
                    let value = self.eval(value)?;
                    dict.insert(key, value);
                }
                Ok(Value::dict(dict))
            }
            ExprP::Dot(object, name) => {
                let object = self.eval(object)?;
                builtins::attribute(object, &name.node, span)
            }
            ExprP::Call(callee, call) => {
                let callee = self.eval(callee)?;
                let args = self.arguments(&call.args)?;
                self.call(callee, args, span)
            }
            ExprP::Index(pair) => {
                let (container, index) = &**pair;
                let container = self.eval(container)?;
                let index = self.eval(index)?;
                builtins::index(&container, &index, span)
            }
            ExprP::Index2(_) => fail(span, "`a[b, c]` isn't supported"),
            ExprP::Slice(container, start, stop, step) => {
                let container = self.eval(container)?;
                let start = self.slice_bound(start.as_deref())?;
                let stop = self.slice_bound(stop.as_deref())?;
                let step = self.slice_bound(step.as_deref())?;
                builtins::slice(&container, start, stop, step, span)
            }
            ExprP::Lambda(lambda) => self.lambda(lambda),
            ExprP::Not(inner) => Ok(Value::Bool(!self.eval(inner)?.truthy())),
            ExprP::Minus(inner) => match self.eval(inner)? {
                Value::Int(n) => n
                    .checked_neg()
                    .map(Value::Int)
                    .map_or_else(|| fail(span, "integer overflow"), Ok),
                Value::Float(f) => Ok(Value::Float(-f)),
                other => fail(span, format!("can't negate a {}", other.type_name())),
            },
            ExprP::Plus(inner) => match self.eval(inner)? {
                number @ (Value::Int(_) | Value::Float(_)) => Ok(number),
                other => fail(
                    span,
                    format!("unary `+` needs a number, not {}", other.type_name()),
                ),
            },
            ExprP::BitNot(inner) => match self.eval(inner)? {
                Value::Int(n) => Ok(Value::Int(!n)),
                other => fail(span, format!("`~` needs an int, not {}", other.type_name())),
            },
            ExprP::Op(left, BinOp::And, right) => {
                let left = self.eval(left)?;
                if left.truthy() {
                    self.eval(right)
                } else {
                    Ok(left)
                }
            }
            ExprP::Op(left, BinOp::Or, right) => {
                let left = self.eval(left)?;
                if left.truthy() {
                    Ok(left)
                } else {
                    self.eval(right)
                }
            }
            ExprP::Op(left, op, right) => {
                let left = self.eval(left)?;
                let right = self.eval(right)?;
                binary(*op, left, right, span)
            }
            ExprP::If(triple) => {
                let (condition, yes, no) = &**triple;
                if self.eval(condition)?.truthy() {
                    self.eval(yes)
                } else {
                    self.eval(no)
                }
            }
            ExprP::ListComprehension(body, first, rest) => {
                let mut out = Vec::new();
                self.comprehension(first, rest, &mut |interp: &mut Interp| {
                    out.push(interp.eval(body)?);
                    check_len(out.len(), span)
                })?;
                Ok(Value::list(out))
            }
            ExprP::DictComprehension(pair, first, rest) => {
                let (key, value) = &**pair;
                let mut dict = Dict::default();
                self.comprehension(first, rest, &mut |interp: &mut Interp| {
                    let key = interp.eval(key)?;
                    if !key.hashable() {
                        return fail(span, format!("a {} can't be a dict key", key.type_name()));
                    }
                    let value = interp.eval(value)?;
                    dict.insert(key, value);
                    check_len(dict.entries.len(), span)
                })?;
                Ok(Value::dict(dict))
            }
            ExprP::FString(_) => fail(span, "f-strings aren't supported"),
        }
    }

    fn eval_all(&mut self, exprs: &[AstExpr]) -> Eval<Vec<Value>> {
        exprs.iter().map(|expr| self.eval(expr)).collect()
    }

    fn slice_bound(&mut self, expr: Option<&AstExpr>) -> Eval<Option<i64>> {
        let Some(expr) = expr else {
            return Ok(None);
        };
        match self.eval(expr)? {
            Value::Int(n) => Ok(Some(n)),
            Value::None => Ok(None),
            other => fail(
                expr.span,
                format!("a slice bound is an int, not {}", other.type_name()),
            ),
        }
    }

    fn comprehension(
        &mut self,
        first: &ForClause,
        rest: &[Clause],
        emit: &mut dyn FnMut(&mut Interp) -> Eval<()>,
    ) -> Eval<()> {
        self.frame_mut().scopes.push(HashMap::new());
        let result = self.clause_for(first, rest, emit);
        self.frame_mut().scopes.pop();
        result
    }

    fn clause_for(
        &mut self,
        clause: &ForClause,
        rest: &[Clause],
        emit: &mut dyn FnMut(&mut Interp) -> Eval<()>,
    ) -> Eval<()> {
        let over = self.eval(&clause.over)?;
        for item in items(&over, clause.over.span)? {
            self.tick(clause.over.span)?;
            self.assign(&clause.var, item, true)?;
            self.clauses(rest, emit)?;
        }
        Ok(())
    }

    fn clauses(
        &mut self,
        rest: &[Clause],
        emit: &mut dyn FnMut(&mut Interp) -> Eval<()>,
    ) -> Eval<()> {
        match rest.split_first() {
            None => emit(self),
            Some((ClauseP::If(condition), tail)) => {
                if self.eval(condition)?.truthy() {
                    self.clauses(tail, emit)
                } else {
                    Ok(())
                }
            }
            Some((ClauseP::For(clause), tail)) => self.clause_for(clause, tail, emit),
        }
    }

    fn arguments(&mut self, args: &[AstArgument]) -> Eval<Args> {
        let mut positional = Vec::new();
        let mut named = Vec::new();
        for arg in args {
            match &arg.node {
                ArgumentP::Positional(expr) => positional.push(self.eval(expr)?),
                ArgumentP::Named(name, expr) => named.push((name.node.clone(), self.eval(expr)?)),
                ArgumentP::Args(expr) => {
                    let value = self.eval(expr)?;
                    positional.extend(collect(&value, expr.span)?);
                }
                ArgumentP::KwArgs(expr) => {
                    let Value::Dict(dict) = self.eval(expr)? else {
                        return fail(expr.span, "`**` needs a dict");
                    };
                    for (key, value) in &dict.borrow().entries {
                        let Value::Str(key) = key else {
                            return fail(expr.span, "`**` needs string keys");
                        };
                        named.push((key.to_string(), value.clone()));
                    }
                }
            }
        }
        Ok(Args { positional, named })
    }

    pub fn call(&mut self, callee: Value, args: Args, span: Span) -> Eval<Value> {
        match callee {
            Value::Function(function) => self.call_function(&function, args, span),
            Value::Builtin(builtin) => builtins::call(self, builtin, args, span),
            Value::Method(object, name) => builtins::method(*object, &name, args, span),
            other => fail(span, format!("a {} isn't callable", other.type_name())),
        }
    }

    fn call_function(&mut self, function: &Function, args: Args, span: Span) -> Eval<Value> {
        if self.calls.contains(&function.id) {
            return fail(
                span,
                format!(
                    "`{}` calls itself, and Starlark has no recursion",
                    function.name
                ),
            );
        }
        if self.calls.len() >= MAX_DEPTH {
            return fail(span, format!("calls nest deeper than {MAX_DEPTH}"));
        }
        let locals = bind_params(function, args, span)?;
        self.frames.push(Frame {
            locals,
            scopes: Vec::new(),
            captured: function.captured.clone(),
        });
        self.calls.push(function.id);
        let result = match &function.body {
            Body::Block(body) => self.exec(body).map(|flow| match flow {
                Flow::Return(value) => value,
                _ => Value::None,
            }),
            Body::Lambda(body) => self.eval(body),
        };
        self.calls.pop();
        self.frames.pop();
        result
    }
}

fn bind_params(function: &Function, args: Args, span: Span) -> Eval<HashMap<String, Value>> {
    let mut locals = HashMap::new();
    let mut positional = args.positional.into_iter();
    let mut named = args.named;
    let mut keyword_only = false;
    let mut take_named = |name: &str| {
        named
            .iter()
            .position(|(n, _)| n == name)
            .map(|index| named.remove(index).1)
    };
    for param in &function.params {
        match param {
            Param::Required(name) | Param::Optional(name, _) => {
                let by_position = if keyword_only {
                    None
                } else {
                    positional.next()
                };
                let by_name = take_named(name);
                let value = match (by_position, by_name) {
                    (Some(_), Some(_)) => {
                        return fail(span, format!("`{}` got `{name}` twice", function.name));
                    }
                    (Some(value), None) | (None, Some(value)) => value,
                    (None, None) => match param {
                        Param::Optional(_, default) => default.clone(),
                        _ => {
                            return fail(
                                span,
                                format!("`{}` needs an argument for `{name}`", function.name),
                            );
                        }
                    },
                };
                locals.insert(name.clone(), value);
            }
            Param::Args(name) => {
                locals.insert(name.clone(), Value::tuple(positional.by_ref().collect()));
                keyword_only = true;
            }
            Param::Kwargs(_) | Param::KeywordOnly => keyword_only = true,
        }
    }
    if positional.next().is_some() {
        return fail(span, format!("`{}` got too many arguments", function.name));
    }
    if let Some(Param::Kwargs(name)) = function
        .params
        .iter()
        .find(|param| matches!(param, Param::Kwargs(_)))
    {
        let mut extra = Dict::default();
        for (key, value) in named.drain(..) {
            extra.insert(Value::str(&key), value);
        }
        locals.insert(name.clone(), Value::dict(extra));
    }
    if let Some((name, _)) = named.first() {
        return fail(
            span,
            format!("`{}` has no parameter `{name}`", function.name),
        );
    }
    Ok(locals)
}

fn literal_value(literal: &AstLiteral, span: Span) -> Eval<Value> {
    match literal {
        AstLiteral::Int(int) => match &int.node {
            TokenInt::I32(n) => Ok(Value::Int(i64::from(*n))),
            TokenInt::BigInt(_) => fail(span, "that number is too large"),
        },
        AstLiteral::Float(float) => Ok(Value::Float(float.node)),
        AstLiteral::String(text) => Ok(Value::str(&text.node)),
        AstLiteral::Bytes(_) => fail(span, "bytes aren't supported"),
        AstLiteral::Ellipsis => fail(span, "`...` isn't supported"),
    }
}
