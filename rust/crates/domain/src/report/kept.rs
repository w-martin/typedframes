use super::*;

// What a snippet keeps of a file, by statement index: top-level statements, the members of the
// site's class when it is in one, and the frame-returning methods of other classes it calls.
pub(super) struct Kept {
    pub(super) tops: BTreeSet<usize>,
    pub(super) class_top: Option<usize>,
    pub(super) class_members: BTreeSet<usize>,
    pub(super) other_classes: BTreeMap<usize, BTreeSet<usize>>,
}

impl Kept {
    // What to keep: top-level statements (functions, or module-level code), plus the
    // members of the site's class when it is in one.
    pub(super) fn around(body: &[Stmt], site_offset: usize) -> Result<Kept, String> {
        let top = body
            .iter()
            .position(|s| contains(s.range(), site_offset))
            .ok_or("the site is not inside any statement")?;
        let mut kept = Kept {
            tops: BTreeSet::new(),
            class_top: None,
            class_members: BTreeSet::new(),
            other_classes: BTreeMap::new(),
        };
        match &body[top] {
            Stmt::FunctionDef(_) => {
                kept.tops.insert(top);
            }
            Stmt::ClassDef(class) => {
                let member = class
                    .body
                    .iter()
                    .position(|s| contains(s.range(), site_offset))
                    .ok_or("the site is not inside a class member")?;
                kept.class_top = Some(top);
                kept.class_members.insert(member);
            }
            _ => {
                for (i, stmt) in body.iter().enumerate() {
                    if !matches!(
                        stmt,
                        Stmt::FunctionDef(_)
                            | Stmt::ClassDef(_)
                            | Stmt::Import(_)
                            | Stmt::ImportFrom(_)
                    ) {
                        kept.tops.insert(i);
                    }
                }
            }
        }
        Ok(kept)
    }

    pub(super) fn class<'b>(&self, body: &'b [Stmt]) -> Option<&'b StmtClassDef> {
        self.class_top.and_then(|i| match &body[i] {
            Stmt::ClassDef(c) => Some(c),
            _ => None,
        })
    }

    // Names called from the kept top-level statements and class members.
    pub(super) fn called_names(
        &self,
        body: &[Stmt],
        source: &str,
        tokens: &[Tok],
    ) -> BTreeSet<String> {
        let mut called: BTreeSet<String> = BTreeSet::new();
        for &i in &self.tops {
            let r = body[i].range();
            called.extend(callee_names(
                source,
                tokens,
                r.start().to_usize(),
                r.end().to_usize(),
            ));
        }
        if let Some(c) = self.class(body) {
            for &m in &self.class_members {
                let r = c.body[m].range();
                called.extend(callee_names(
                    source,
                    tokens,
                    r.start().to_usize(),
                    r.end().to_usize(),
                ));
            }
        }
        called
    }

    // `called_names`, and the names the kept methods of other classes call.
    fn wanted_names(&self, body: &[Stmt], source: &str, tokens: &[Tok]) -> BTreeSet<String> {
        let mut wanted = self.called_names(body, source, tokens);
        for (&i, members) in &self.other_classes {
            let Stmt::ClassDef(c) = &body[i] else {
                continue;
            };
            for &m in members {
                let r = c.body[m].range();
                wanted.extend(callee_names(
                    source,
                    tokens,
                    r.start().to_usize(),
                    r.end().to_usize(),
                ));
            }
        }
        wanted
    }

    // Pull in same-file functions and class members the kept code calls.
    pub(super) fn add_callees(&mut self, body: &[Stmt], source: &str, tokens: &[Tok]) {
        let mut added = 0;
        loop {
            let wanted = self.wanted_names(body, source, tokens);
            let grew = self.grow_functions(body, &wanted, &mut added)
                | self.grow_class_members(body, &wanted, &mut added)
                | self.grow_other_classes(body, source, &wanted, &mut added);
            if !grew {
                break;
            }
        }
    }

    fn grow_functions(
        &mut self,
        body: &[Stmt],
        wanted: &BTreeSet<String>,
        added: &mut usize,
    ) -> bool {
        let mut grew = false;
        for (i, stmt) in body.iter().enumerate() {
            if let Stmt::FunctionDef(f) = stmt {
                if *added < MAX_HELPERS
                    && !self.tops.contains(&i)
                    && wanted.contains(f.name.as_str())
                {
                    self.tops.insert(i);
                    *added += 1;
                    grew = true;
                }
            }
        }
        grew
    }

    fn grow_class_members(
        &mut self,
        body: &[Stmt],
        wanted: &BTreeSet<String>,
        added: &mut usize,
    ) -> bool {
        let Some(c) = self.class(body) else {
            return false;
        };
        let mut grew = false;
        for (m, stmt) in c.body.iter().enumerate() {
            if let Stmt::FunctionDef(f) = stmt {
                if *added < MAX_HELPERS
                    && !self.class_members.contains(&m)
                    && wanted.contains(f.name.as_str())
                {
                    self.class_members.insert(m);
                    *added += 1;
                    grew = true;
                }
            }
        }
        grew
    }

    // Other top-level classes with a method the kept code calls (`sut.predict(...)`), with just
    // those methods: what the checker needs to know which library a frame the method returns
    // belongs to.
    fn grow_other_classes(
        &mut self,
        body: &[Stmt],
        source: &str,
        wanted: &BTreeSet<String>,
        added: &mut usize,
    ) -> bool {
        let mut grew = false;
        for (i, stmt) in body.iter().enumerate() {
            let Stmt::ClassDef(c) = stmt else {
                continue;
            };
            if self.class_top == Some(i) {
                continue;
            }
            for (m, member) in c.body.iter().enumerate() {
                let Stmt::FunctionDef(f) = member else {
                    continue;
                };
                // Only a method that says it returns a frame can decide the library.
                let returns_frame = f.returns.as_deref().is_some_and(|r| {
                    let text = &source[r.range().start().to_usize()..r.range().end().to_usize()];
                    text.contains("DataFrame") || text.contains("LazyFrame")
                });
                if *added < MAX_HELPERS
                    && returns_frame
                    && wanted.contains(f.name.as_str())
                    && self.other_classes.entry(i).or_default().insert(m)
                {
                    *added += 1;
                    grew = true;
                }
            }
        }
        grew
    }

    // Every name the kept statements and classes mention.
    pub(super) fn referenced_names(
        &self,
        body: &[Stmt],
        source: &str,
        tokens: &[Tok],
    ) -> BTreeSet<String> {
        let mut referenced: BTreeSet<String> = BTreeSet::new();
        for i in self
            .tops
            .iter()
            .copied()
            .chain(self.class_top)
            .chain(self.other_classes.keys().copied())
        {
            let r = body[i].range();
            referenced.extend(names_in(
                source,
                tokens,
                r.start().to_usize(),
                r.end().to_usize(),
            ));
        }
        referenced
    }

    // The kept code, in source order.
    pub(super) fn redact_into(
        &self,
        redactor: &mut Redactor,
        body: &[Stmt],
        source: &str,
        tokens: &[Tok],
    ) {
        for (i, stmt) in body.iter().enumerate() {
            if self.tops.contains(&i) {
                redactor.out.push_str("\n\n");
                redactor.redact(
                    stmt.range().start().to_usize(),
                    stmt.range().end().to_usize(),
                );
                redactor.out.push('\n');
            } else if self.class_top == Some(i) {
                let c = self.class(body).expect("class_top implies a class");
                redact_class(redactor, source, tokens, stmt, c, &self.class_members);
            } else if let (Some(members), Stmt::ClassDef(c)) = (self.other_classes.get(&i), stmt) {
                redact_class(redactor, source, tokens, stmt, c, members);
            }
        }
    }
}
