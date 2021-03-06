use std::{
    convert::TryFrom,
    collections::HashSet,
};

use crate::common::span::{Span, Spanned};

use crate::compiler::{
    rule::Rule,
    ast::{AST, ASTPattern, ArgPat},
    cst::{CST, CSTPattern},
    syntax::Syntax
};

// TODO: separate macro step and desugaring into two different steps?

pub fn desugar(ast: Spanned<AST>) -> Result<Spanned<CST>, Syntax> {
    let mut transformer = Transformer::new();
    let cst = transformer.walk(ast)?;
    return Ok(cst);
}

/// Applies compile-time transformations to the AST
pub struct Transformer {
    rules: Vec<Spanned<Rule>>,
}

impl Transformer {
    pub fn new() -> Transformer {
        Transformer { rules: vec![] }
    }

    /// desugars an AST into a CST
    /// This function will become more complicated later on
    /// once macros are introduced, but right now it's basically a 1 to 1 translation
    pub fn walk(&mut self, ast: Spanned<AST>) -> Result<Spanned<CST>, Syntax> {
        let cst: CST = match ast.item {
            AST::Symbol(_) => self.symbol(ast.clone())?,
            AST::Data(d) => CST::Data(d),
            AST::Block(b) => self.block(b)?,
            AST::Form(f) => self.form(f)?,
            AST::Composition { argument, function } => self.composition(*argument, *function)?,
            AST::Pattern(_) => return Err(Syntax::error("Unexpected pattern", &ast.span)),
            AST::ArgPat(_)  => return Err(Syntax::error("Unexpected argument pattern", &ast.span)),
            AST::Syntax { arg_pat, expression } => self.rule(*arg_pat, *expression)?,
            AST::Assign { pattern, expression } => self.assign(*pattern, *expression)?,
            AST::Lambda { pattern, expression } => self.lambda(*pattern, *expression)?,
            AST::Print(e) => CST::Print(Box::new(self.walk(*e)?)),
            AST::Label(n, e) => CST::Label(n, Box::new(self.walk(*e)?)),
        };

        return Ok(Spanned::new(cst, ast.span))
    }

    pub fn symbol(&mut self, ast: Spanned<AST>) -> Result<CST, Syntax> {
        self.form(vec![ast])
    }

    /// Recursively build up a call from a flat form.
    /// Basically turns `(a b c d)` into `(((a b) c) d)`.
    pub fn call(&mut self, mut f: Vec<Spanned<AST>>) -> Result<CST, Syntax> {
        // TODO: clean up nested logic.
        match f.len() {
            0 => unreachable!("A call must have at least two values - a function and an expression"),
            1 => match f.pop().unwrap().item {
                AST::Symbol(name) => Ok(CST::Symbol(name)),
                _ => unreachable!("A non-symbol call of length 1 is can not be constructed")
            },
            2 => {
                let arg = f.pop().unwrap();
                let fun = f.pop().unwrap();
                Ok(CST::call(self.walk(fun)?, self.walk(arg)?))
            },
            _higher => {
                let arg = self.walk(f.pop().unwrap())?;
                let f_span = Span::join(f.iter().map(|e| e.span.clone()).collect::<Vec<Span>>());
                Ok(CST::call(Spanned::new(self.call(f)?, f_span), arg))
            },
        }
    }

    // TODO: do raw apply and check once macros get more complicated.
    // TODO: Make it possible for forms with less than one value to exist
    pub fn form(&mut self, form: Vec<Spanned<AST>>) -> Result<CST, Syntax> {
        // collect all in-scope pseudokeywords
        let mut keywords = HashSet::new();
        for rule in self.rules.iter() {
            for keyword in Rule::keywords(&rule.item.arg_pat) {
                keywords.insert(keyword);
            }
        }

        // TODO: convert symbols to in-scope pseudokeywords
        // This allows us to error on an imperfect macro match.
        // form = form.iter()
        //     .map(|branch| match branch.item {
        //         AST::Symbol(name)
        //     })

        // build up a list of rules that matched the current form
        // note that this should be 1
        // 0 means that there's a function call
        // more than 1 means there's some macro ambiguity that needs to be resolved
        let mut matches = vec![];
        for rule in self.rules.iter() {
            let mut reversed_remaining = form.clone().into_iter().rev().collect();
            let possibility = Rule::bind(&rule.item.arg_pat, &mut reversed_remaining);

            if let Some(bindings) = possibility {
                if reversed_remaining.is_empty() {
                    matches.push((rule, bindings?))
                }
            }
        }

        if matches.len() == 0 { return self.call(form); }
        if matches.len() > 1 {
            // TODO: make the error prettier
            // might have to rework Syntax a bit...
            return Err(Syntax::error(
                &format!(
                    "This form matched multiple macros:\n\n{}\
                    Note: A form may only match one macro, this must be unambiguious;\n\
                    Try using variable names different than those of pseudokeywords currently in scope,\n\
                    Adjusting the definitions of locally-defined macros,\n\
                    or using parenthesis '( ... )' or curlies '{{ ... }}' to group nested macros",
                    matches.iter()
                        .map(|s| format!("{}", s.0.span))
                        .collect::<Vec<String>>()
                        .join(""),
                ),
                &Spanned::build(&form),
            ))
        }

        let (rule, mut bindings) = matches.pop().unwrap();
        let expanded = Rule::expand(rule.item.tree.clone(), &mut bindings)?;
        return Ok(self.walk(expanded)?.item);
    }

    /// Desugar a function composition.
    /// A composition takes the form `c |> b |> a`
    /// and is left-associative `(c |> b) |> a`.
    /// When desugared, the above is equivalent to the call `a b c`.
    pub fn composition(&mut self, argument: Spanned<AST>, function: Spanned<AST>) -> Result<CST, Syntax> {
        Ok(CST::call(self.walk(function)?, self.walk(argument)?))
    }

    pub fn block(&mut self, b: Vec<Spanned<AST>>) -> Result<CST, Syntax> {
        let mut expressions = vec![];
        for e in b {
            expressions.push(self.walk(e)?)
        }

        Ok(CST::Block(expressions))
    }

    /// TODO: implement full pattern matching
    pub fn assign(&mut self, p: Spanned<ASTPattern>, e: Spanned<AST>) -> Result<CST, Syntax> {
        let p_span = p.span.clone();

        Ok(CST::assign(
            p.map(CSTPattern::try_from)
                .map_err(|err| Syntax::error(&err, &p_span))?,
            self.walk(e)?
        ))
    }

    /// TODO: implement full pattern matching
    pub fn lambda(&mut self, p: Spanned<ASTPattern>, e: Spanned<AST>) -> Result<CST, Syntax> {
        let p_span = p.span.clone();
        let arguments = if let ASTPattern::Chain(c) = p.item { c } else { vec![p] };
        let mut expression = self.walk(e)?;

        for argument in arguments.into_iter().rev() {
            let pattern = argument.map(CSTPattern::try_from)
                .map_err(|err| Syntax::error(&err, &p_span))?;

            let combined = Span::combine(&pattern.span, &expression.span);
            expression   = Spanned::new(CST::lambda(pattern, expression), combined);
        }

        return Ok(expression.item);
    }

    pub fn rule(&mut self, arg_pat: Spanned<ArgPat>, tree: Spanned<AST>) -> Result<CST, Syntax> {
        let patterns_span = arg_pat.span.clone();
        let rule = Rule::new(arg_pat, tree)?;
        self.rules.push(Spanned::new(rule, patterns_span));

        // TODO: return nothing?
        Ok(CST::Block(vec![]))
    }
}
