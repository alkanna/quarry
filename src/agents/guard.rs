//! The inference guard. Scrubbing changes values in what leaves Quarry, but a query can still learn or copy a value
//! without returning it under its own key. Field rules come in two modes:
//!
//! - Hide (secrets): a regex probe, a `$gt` binary search or a sort would reveal the value, so every reference is
//!   refused, and comparing or computing on an ancestor is refused too.
//! - MaskOutput (personal data): agents may filter, sort, join on and write the field, as looking a customer up by
//!   email is the job. What they may not do is move its value under another key or out of its document (computed
//!   fields, group keys, `$replaceRoot`, `distinct`), where the scrubber, which works by key, no longer sees it.
//!
//! Whole-document constructs ($$ROOT, $objectToArray, computed $getField, $function) carry every field under keys
//! no rule knows, so they are refused wherever any field rule applies. Free SQL, scripts and the shell cannot be
//! walked; the policy refuses them where Hide rules reach.

use super::scrub::{NsRules, Rules};
use super::tools::{Call, Target, Update, each_stage, stage_target};
#[cfg(test)]
use crate::db::sql::Dialect;
#[cfg(test)]
use crate::db::sql::lex::{Tok, tokens};
use mongodb::bson::{Bson, Document};

type Res = Result<(), String>;

fn refuse<T>(msg: String) -> Result<T, String> {
    Err(msg)
}

/// Query operators that compare a field's whole value, so they may not run on a field that holds hidden ones.
const COMPARE: [&str; 23] = ["$eq", "$ne", "$gt", "$gte", "$lt", "$lte", "$in", "$nin", "$all", "$regex", "$options", "$mod", "$bitsAllSet", "$bitsAnySet", "$bitsAllClear", "$bitsAnyClear", "$geoWithin", "$geoIntersects", "$near", "$nearSphere", "$maxDistance", "$minDistance", "$geometry"];

/// Aggregation expression operators that only compute on their arguments. Anything that reads the document as a
/// whole or runs code ($objectToArray, $function, $accumulator) is missing on purpose.
const EXPR: &[&str] = &[
    "$abs", "$add", "$ceil", "$divide", "$exp", "$floor", "$ln", "$log", "$log10", "$mod", "$multiply", "$pow", "$round", "$sqrt", "$subtract", "$trunc",
    "$eq", "$ne", "$gt", "$gte", "$lt", "$lte", "$cmp", "$and", "$or", "$not", "$cond", "$ifNull", "$switch",
    "$concat", "$toLower", "$toUpper", "$substr", "$substrCP", "$substrBytes", "$strLenCP", "$strLenBytes", "$trim", "$ltrim", "$rtrim", "$split", "$indexOfCP", "$indexOfBytes", "$regexMatch", "$regexFind", "$regexFindAll", "$replaceOne", "$replaceAll", "$strcasecmp",
    "$toString", "$toInt", "$toLong", "$toDouble", "$toDecimal", "$toDate", "$toObjectId", "$toBool", "$convert", "$type", "$isNumber",
    "$year", "$month", "$dayOfMonth", "$dayOfWeek", "$dayOfYear", "$hour", "$minute", "$second", "$millisecond", "$week", "$isoWeek", "$isoWeekYear", "$isoDayOfWeek", "$dateToString", "$dateFromString", "$dateAdd", "$dateSubtract", "$dateDiff", "$dateTrunc", "$dateFromParts", "$dateToParts",
    "$size", "$arrayElemAt", "$first", "$last", "$in", "$slice", "$concatArrays", "$filter", "$map", "$reduce", "$range", "$reverseArray", "$indexOfArray", "$isArray", "$sortArray", "$zip", "$allElementsTrue", "$anyElementTrue", "$setUnion", "$setIntersection", "$setDifference", "$setEquals", "$setIsSubset",
    "$let", "$sum", "$avg", "$min", "$max", "$mergeObjects", "$arrayToObject", "$push", "$addToSet", "$stdDevPop", "$stdDevSamp", "$count", "$top", "$bottom", "$topN", "$bottomN", "$firstN", "$lastN", "$maxN", "$minN", "$median", "$percentile",
];

const UPDATE_OPS: [&str; 15] = ["$set", "$unset", "$inc", "$mul", "$min", "$max", "$rename", "$push", "$pull", "$addToSet", "$pop", "$currentDate", "$setOnInsert", "$pullAll", "$bit"];

/// Commands run_command passes without a field check: none returns field values.
const SAFE_COMMANDS: [&str; 17] = ["ping", "hello", "isMaster", "buildInfo", "listCollections", "listDatabases", "listIndexes", "collStats", "dbStats", "dataSize", "validate", "create", "drop", "createIndexes", "dropIndexes", "collMod", "connectionStatus"];

#[derive(Clone, Copy, PartialEq)]
enum Use {
    /// A filter, sort or join key: masked fields pass, hidden ones do not.
    Pred,
    /// A value that lands under another key or out of its document: neither masked nor hidden fields pass.
    Out,
}

fn hidden_msg(path: &str) -> String {
    format!("`{path}` is hidden by a data rule on this collection, so agents cannot query, sort or change it")
}

fn masked_msg(path: &str) -> String {
    format!("`{path}` is masked by a data rule: agents can filter, sort and join on it, but not copy its value into another field or group by it")
}

fn join(prefix: &str, k: &str) -> String {
    if prefix.is_empty() { k.to_string() } else { format!("{prefix}.{k}") }
}

/// The rules of one namespace, with the whole rule set and database to check what its pipelines join.
struct G<'a> {
    r: NsRules<'a>,
    rules: &'a Rules,
    db: String,
    /// Any field rule applies here, so operator allow-lists and the whole-document ban are on.
    any: bool,
}

impl<'a> G<'a> {
    fn new(rules: &'a Rules, db: &str, coll: &str) -> G<'a> {
        let r = rules.for_ns(db, coll);
        let any = r.field_rules();
        G { r, rules, db: db.to_string(), any }
    }

    fn other(&self, t: &Target) -> G<'a> {
        G::new(self.rules, &t.db, t.coll.as_deref().unwrap_or(""))
    }

    fn pred(&self, path: &str) -> Res {
        if self.r.hides(path) {
            return refuse(hidden_msg(path));
        }
        if self.r.hides_below(path) {
            return refuse(if path.is_empty() { "Reading the whole document is not allowed where rules hide fields".into() } else { format!("`{path}` holds fields hidden by a data rule, so agents cannot read it as a whole") });
        }
        Ok(())
    }

    fn out(&self, path: &str) -> Res {
        self.pred(path)?;
        if self.r.masks(path) {
            return refuse(masked_msg(path));
        }
        if self.r.masks_below(path) {
            return refuse(if path.is_empty() { "Copying the whole document is not allowed where rules mask fields".into() } else { format!("`{path}` holds masked fields, so agents cannot copy it into another field or group by it") });
        }
        Ok(())
    }

    fn reference(&self, path: &str, u: Use) -> Res {
        if u == Use::Pred { self.pred(path) } else { self.out(path) }
    }

    fn whole(&self, what: &str) -> Res {
        if self.any { refuse(format!("{what} reads the whole document, which is not allowed where data rules apply")) } else { Ok(()) }
    }

    fn unknown(&self, what: String) -> Res {
        if self.any { refuse(format!("{what} is not allowed where data rules apply")) } else { Ok(()) }
    }

    fn filter(&self, f: &Document, prefix: &str) -> Res {
        for (k, v) in f {
            match k.as_str() {
                "$and" | "$or" | "$nor" => {
                    let Bson::Array(a) = v else { return refuse(format!("{k} needs an array")) };
                    for c in a {
                        let Bson::Document(d) = c else { return refuse(format!("{k} needs an array of conditions")) };
                        self.filter(d, prefix)?;
                    }
                }
                "$expr" => self.expr(v, Use::Pred)?,
                "$comment" => {}
                "$where" | "$function" | "$jsonSchema" | "$text" => self.whole(k)?,
                o if o.starts_with('$') => self.unknown(format!("The query operator {o}"))?,
                path => self.cond(&join(prefix, path), v)?,
            }
        }
        Ok(())
    }

    /// A condition on one field. Masked fields take any operator. On a field holding hidden ones, $exists, $type,
    /// $size and $elemMatch over sub-fields may run, since they never compare its whole value.
    fn cond(&self, path: &str, v: &Bson) -> Res {
        if self.r.hides(path) {
            return refuse(hidden_msg(path));
        }
        let whole = || if self.r.hides_below(path) { self.pred(path) } else { Ok(()) };
        match v {
            Bson::Document(d) if d.keys().next().is_some_and(|k| k.starts_with('$')) => {
                for (op, arg) in d {
                    match op.as_str() {
                        "$exists" | "$type" | "$size" => {}
                        "$elemMatch" => {
                            let Bson::Document(m) = arg else { return refuse("$elemMatch needs an object".into()) };
                            if m.keys().all(|k| k.starts_with('$') && !matches!(k.as_str(), "$and" | "$or" | "$nor")) {
                                self.cond(path, arg)?;
                            } else {
                                self.filter(m, path)?;
                            }
                        }
                        "$not" => match arg {
                            Bson::Document(_) => self.cond(path, arg)?,
                            _ => whole()?,
                        },
                        o if COMPARE.contains(&o) => {
                            whole()?;
                            self.literal(path, arg)?;
                        }
                        o => self.unknown(format!("The query operator {o}"))?,
                    }
                }
                Ok(())
            }
            _ => {
                whole()?;
                self.literal(path, v)
            }
        }
    }

    /// A value compared with or written to a field: an embedded document's keys are fields too, so
    /// `{customer: {ssn: ...}}` names `customer.ssn`.
    fn literal(&self, path: &str, v: &Bson) -> Res {
        match v {
            Bson::Document(d) => d.iter().try_for_each(|(k, x)| {
                let p = if k.starts_with('$') { path.to_string() } else { join(path, k) };
                if self.r.hides(&p) { refuse(hidden_msg(&p)) } else { self.literal(&p, x) }
            }),
            Bson::Array(a) => a.iter().try_for_each(|x| self.literal(path, x)),
            _ => Ok(()),
        }
    }

    fn sort(&self, s: &Document) -> Res {
        for (k, v) in s {
            self.pred(k)?;
            match v {
                Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => {}
                Bson::Document(d) if d.keys().all(|k| k == "$meta") => {}
                _ => return refuse(format!("Sort on `{k}` must be 1 or -1")),
            }
        }
        Ok(())
    }

    /// Including or excluding a field keeps its key, so the scrubber still finds it in the output; computing from a
    /// field does not.
    fn projection(&self, p: &Document) -> Res {
        for (k, v) in p {
            match v {
                v if plain_flag(v) => {}
                Bson::Document(d) if d.len() == 1 && d.contains_key("$elemMatch") => self.cond(k.trim_end_matches(".$"), v)?,
                Bson::Document(d) if d.len() == 1 && (d.contains_key("$slice") || d.contains_key("$meta")) => {}
                Bson::Document(d) if d.keys().next().is_some_and(|k| !k.starts_with('$')) => self.projection(d)?,
                other => self.expr(other, Use::Out)?,
            }
        }
        Ok(())
    }

    /// An aggregation expression: field references are checked for their use, operators must be on the allow-list.
    fn expr(&self, v: &Bson, u: Use) -> Res {
        match v {
            Bson::String(s) if s.starts_with("$$") => {
                let (var, rest) = s[2..].split_once('.').unwrap_or((&s[2..], ""));
                if var == "ROOT" || var == "CURRENT" {
                    if rest.is_empty() { self.whole(&format!("$${var}")) } else { self.reference(rest, u) }
                } else if !rest.is_empty() && (self.r.hides(rest) || (u == Use::Out && self.r.masks(rest))) {
                    refuse(if self.r.hides(rest) { hidden_msg(rest) } else { masked_msg(rest) })
                } else {
                    Ok(())
                }
            }
            Bson::String(s) if s.starts_with('$') => self.reference(&s[1..], u),
            Bson::Array(a) => a.iter().try_for_each(|x| self.expr(x, u)),
            Bson::Document(d) => match d.keys().next() {
                Some(op) if op.starts_with('$') => {
                    if d.len() != 1 {
                        return refuse(format!("An expression object holds one operator, not {}", d.len()));
                    }
                    let arg = d.get(op).expect("first key");
                    match op.as_str() {
                        "$literal" => Ok(()),
                        "$getField" | "$setField" | "$unsetField" => self.get_field(op, arg, u),
                        "$objectToArray" | "$function" | "$accumulator" | "$where" => self.whole(op).and_then(|_| self.expr(arg, u)),
                        o if EXPR.contains(&o) => self.expr(arg, u),
                        o => self.unknown(format!("The expression operator {o}")).and_then(|_| self.expr(arg, u)),
                    }
                }
                _ => d.values().try_for_each(|x| self.expr(x, u)),
            },
            _ => Ok(()),
        }
    }

    /// $getField and its kin read a field by a name that may be computed; only a literal name from a plain input
    /// passes, checked as a reference.
    fn get_field(&self, op: &str, arg: &Bson, u: Use) -> Res {
        let (field, input) = match arg {
            Bson::String(f) => (Some(f.as_str()), None),
            Bson::Document(d) => (d.get_str("field").ok(), d.get("input")),
            _ => (None, None),
        };
        let Some(field) = field.filter(|f| !f.starts_with('$')) else { return self.whole(&format!("{op} with a computed name")) };
        if let Bson::Document(d) = arg
            && let Some(v) = d.get("value")
        {
            self.expr(v, Use::Out)?;
        }
        match input {
            None => self.reference(field, u),
            Some(Bson::String(s)) if s == "$$ROOT" || s == "$$CURRENT" => self.reference(field, u),
            Some(Bson::String(s)) if s.starts_with('$') && !s.starts_with("$$") => self.reference(&join(&s[1..], field), u),
            Some(other) => {
                self.expr(other, u)?;
                if self.r.hides(field) || (u == Use::Out && self.r.masks(field)) { refuse(masked_msg(field)) } else { Ok(()) }
            }
        }
    }

    /// $merge's whenMatched pipeline reads the target's own fields and $out replaces them, so a target with data
    /// rules could get hidden values copied under keys no rule knows.
    fn out_target(&self, p: &[Document]) -> Res {
        let last = p.last().and_then(|s| s.iter().find(|(k, _)| *k == "$out" || *k == "$merge"));
        match last.and_then(|(k, v)| stage_target(&self.db, k, v)) {
            Some(t) if self.other(&t).any => refuse(format!("Data rules apply to {}.{}, which $out or $merge would write into", t.db, t.coll.as_deref().unwrap_or(""))),
            _ => Ok(()),
        }
    }

    /// Every stage of a pipeline, then the stage checks. $out and $merge are allowed only as the last stage of
    /// aggregate_into; server stages never.
    fn pipeline(&self, p: &[Document], into: bool) -> Res {
        let (mut outs, mut server) = (0, None);
        each_stage(p, &mut |name, _| {
            if name == "$out" || name == "$merge" {
                outs += 1;
            }
            if matches!(name, "$currentOp" | "$listSessions" | "$listLocalSessions") {
                server = Some(name.to_string());
            }
        });
        if let Some(s) = server {
            return refuse(format!("{s} reads server internals and is not allowed for agents"));
        }
        let ends_in_out = into && p.last().is_some_and(|s| s.contains_key("$out") || s.contains_key("$merge"));
        if outs > usize::from(ends_in_out) {
            return refuse(if into { "Only the last stage may be $out or $merge".into() } else { "$out and $merge write data; use aggregate_into".into() });
        }
        self.stages(p)
    }

    fn stages(&self, p: &[Document]) -> Res {
        for stage in p {
            for (name, body) in stage {
                self.stage(name, body)?;
            }
        }
        Ok(())
    }

    fn stage<'b>(&self, name: &str, body: &'b Bson) -> Res {
        let doc = |b: &'b Bson| as_doc(name, b);
        match name {
            "$match" => self.filter(doc(body)?, ""),
            "$sort" => self.sort(doc(body)?),
            "$limit" | "$skip" | "$sample" | "$count" | "$unset" | "$out" | "$merge" | "$collStats" | "$indexStats" => Ok(()),
            "$project" => self.projection(doc(body)?),
            "$addFields" | "$set" => doc(body)?.values().try_for_each(|e| self.expr(e, Use::Out)),
            "$unwind" => {
                let p = match body {
                    Bson::Document(d) => d.get("path"),
                    v => Some(v),
                };
                // Unwinding keeps the field's key, so masked and hidden values inside it are still scrubbed.
                match p.and_then(path_ref) {
                    Some(p) if self.r.hides(p) => refuse(hidden_msg(p)),
                    Some(_) => Ok(()),
                    None => refuse("$unwind needs a field path".into()),
                }
            }
            "$group" => self.group(doc(body)?, "_id"),
            "$bucket" | "$bucketAuto" => {
                let d = doc(body)?;
                self.expr(d.get("groupBy").unwrap_or(&Bson::Null), Use::Out)?;
                d.get_document("output").map_or(Ok(()), |o| self.group(o, ""))
            }
            "$sortByCount" => self.expr(body, Use::Out),
            "$replaceRoot" => self.expr(doc(body)?.get("newRoot").unwrap_or(&Bson::Null), Use::Out),
            "$replaceWith" => self.expr(body, Use::Out),
            "$facet" => doc(body)?.values().try_for_each(|sub| match sub {
                Bson::Array(a) => self.stages(&a.iter().filter_map(|s| s.as_document().cloned()).collect::<Vec<_>>()),
                _ => refuse("$facet needs arrays of stages".into()),
            }),
            "$lookup" | "$graphLookup" | "$unionWith" => self.join_stage(name, body),
            "$geoNear" => doc(body)?.get_document("query").map_or(Ok(()), |q| self.filter(q, "")),
            "$redact" => self.whole(name),
            other => self.unknown(format!("The stage {other}")),
        }
    }

    /// $group keys and accumulators land under new keys, so they take only fields that are neither masked nor
    /// hidden; counting takes nothing.
    fn group(&self, g: &Document, id_key: &str) -> Res {
        for (k, v) in g {
            if k == id_key {
                self.expr(v, Use::Out)?;
                continue;
            }
            let Bson::Document(acc) = v else { return refuse(format!("`{k}` needs an accumulator")) };
            for (op, arg) in acc {
                match (op.as_str(), arg) {
                    ("$count", _) => {}
                    ("$sum", Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)) => {}
                    ("$accumulator" | "$function", _) => self.whole(op)?,
                    (o, a) if EXPR.contains(&o) => self.expr(a, Use::Out)?,
                    (o, _) => self.unknown(format!("The accumulator {o}"))?,
                }
            }
        }
        Ok(())
    }

    /// A join reads another namespace: its own rules apply to its fields and sub-pipeline. A namespace with field
    /// path rules is refused, since its documents land under `as`, where path rules no longer match.
    fn join_stage(&self, name: &str, body: &Bson) -> Res {
        let Some(t) = stage_target(&self.db, name, body) else { return refuse(format!("{name} needs a collection")) };
        let target = self.other(&t);
        if target.r.path_rules_here() {
            return refuse(format!("{name} reads {}.{}, where field path rules apply", t.db, t.coll.as_deref().unwrap_or("")));
        }
        let Bson::Document(d) = body else { return Ok(()) };
        let sub = |k: &str| d.get_array(k).map(|a| a.iter().filter_map(|s| s.as_document().cloned()).collect::<Vec<_>>()).unwrap_or_default();
        match name {
            "$lookup" => {
                if let Ok(l) = d.get_str("localField") {
                    self.pred(l)?;
                }
                if let Ok(f) = d.get_str("foreignField") {
                    target.pred(f)?;
                }
                if let Ok(vars) = d.get_document("let") {
                    vars.values().try_for_each(|e| self.expr(e, Use::Out))?;
                }
                target.stages(&sub("pipeline"))
            }
            "$graphLookup" => {
                self.expr(d.get("startWith").unwrap_or(&Bson::Null), Use::Pred)?;
                for k in ["connectFromField", "connectToField"] {
                    if let Ok(f) = d.get_str(k) {
                        target.pred(f)?;
                    }
                }
                d.get_document("restrictSearchWithMatch").map_or(Ok(()), |m| target.filter(m, ""))
            }
            _ => target.stages(&sub("pipeline")),
        }
    }

    /// Writes: a hidden field may not be written at all, since even a literal `$set` tells by its modified count
    /// whether the value was already equal. Masked fields take literal writes; copying them ($rename, update
    /// pipelines) is refused.
    fn update_doc(&self, d: &Document) -> Res {
        for (op, fields) in d {
            if !UPDATE_OPS.contains(&op.as_str()) {
                self.unknown(format!("The update operator {op}"))?;
            }
            let Bson::Document(f) = fields else { return refuse(format!("{op} needs an object")) };
            for (path, v) in f {
                self.pred(path)?;
                if op == "$rename" {
                    let Bson::String(to) = v else { return refuse("$rename needs field names".into()) };
                    self.out(path)?;
                    self.pred(to)?;
                }
                self.literal(path, v)?;
            }
        }
        Ok(())
    }

    fn update_pipeline(&self, p: &[Document]) -> Res {
        for stage in p {
            for (name, body) in stage {
                match (name.as_str(), body) {
                    ("$set" | "$addFields", Bson::Document(d)) => d.iter().try_for_each(|(k, e)| self.pred(k).and_then(|_| self.expr(e, Use::Out)))?,
                    ("$project", Bson::Document(d)) => d.iter().try_for_each(|(k, e)| self.pred(k).and_then(|_| if plain_flag(e) { Ok(()) } else { self.expr(e, Use::Out) }))?,
                    ("$unset", Bson::String(k)) => self.pred(k)?,
                    ("$unset", Bson::Array(a)) => a.iter().try_for_each(|k| k.as_str().map_or_else(|| refuse("$unset needs field names".into()), |k| self.pred(k)))?,
                    ("$replaceRoot" | "$replaceWith", _) => self.whole(name)?,
                    _ => self.unknown(format!("{name} in an update pipeline"))?,
                }
            }
        }
        Ok(())
    }

    /// Inserted documents may not set hidden fields; masked ones are fine.
    fn written(&self, d: &Document, prefix: &str) -> Res {
        for (k, v) in d {
            let p = join(prefix, k);
            if self.r.hides(&p) {
                return refuse(hidden_msg(&p));
            }
            match v {
                Bson::Document(sub) => self.written(sub, &p)?,
                Bson::Array(a) => a.iter().filter_map(Bson::as_document).try_for_each(|sub| self.written(sub, &p))?,
                _ => {}
            }
        }
        Ok(())
    }

    fn column(&self, c: &str) -> Res {
        if self.r.hides(c) { refuse(format!("Column `{c}` is hidden by a data rule, so agents cannot filter, sort or write it")) } else { Ok(()) }
    }

    /// The reading commands go through the same checks as their tools; anything else must be on the allow-list.
    fn command(&self, c: &Document) -> Res {
        let Some((name, target)) = c.iter().next() else { return refuse("Empty command".into()) };
        let coll = target.as_str().unwrap_or("");
        let g = G::new(self.rules, &self.db, coll);
        match name.as_str() {
            "find" => {
                c.get_document("filter").map_or(Ok(()), |f| g.filter(f, ""))?;
                c.get_document("sort").map_or(Ok(()), |s| g.sort(s))?;
                c.get_document("projection").map_or(Ok(()), |p| g.projection(p))
            }
            "aggregate" => {
                let p: Vec<Document> = c.get_array("pipeline").map(|a| a.iter().filter_map(|s| s.as_document().cloned()).collect()).unwrap_or_default();
                let into = p.last().is_some_and(|s| s.contains_key("$out") || s.contains_key("$merge"));
                if into && g.any {
                    return refuse("Data rules apply to this collection, and $out or $merge would copy its fields unscrubbed".into());
                }
                if into {
                    g.out_target(&p)?;
                }
                g.pipeline(&p, into)
            }
            // A view reads its source under its own name, where the source's rules no longer match.
            n if (n.eq_ignore_ascii_case("create") || n.eq_ignore_ascii_case("collMod")) && c.contains_key("viewOn") => {
                let src = G::new(self.rules, &self.db, c.get_str("viewOn").unwrap_or(""));
                if src.any {
                    return refuse(format!("Data rules apply to {}.{}, so agents cannot make a view of it", self.db, c.get_str("viewOn").unwrap_or("")));
                }
                let p: Vec<Document> = c.get_array("pipeline").map(|a| a.iter().filter_map(|s| s.as_document().cloned()).collect()).unwrap_or_default();
                src.pipeline(&p, false)
            }
            "count" => c.get_document("query").map_or(Ok(()), |q| g.filter(q, "")),
            "distinct" => {
                g.out(c.get_str("key").unwrap_or(""))?;
                c.get_document("query").map_or(Ok(()), |q| g.filter(q, ""))
            }
            "explain" => c.get_document("explain").map_or(Ok(()), |inner| self.command(inner)),
            n if SAFE_COMMANDS.iter().any(|s| s.eq_ignore_ascii_case(n)) => Ok(()),
            n => refuse(format!("The {n} command is not available to agents; use the tools")),
        }
    }
}

fn as_doc<'b>(name: &str, b: &'b Bson) -> Result<&'b Document, String> {
    b.as_document().ok_or_else(|| format!("{name} needs an object"))
}

fn plain_flag(v: &Bson) -> bool {
    matches!(v, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Boolean(_))
}

fn path_ref(v: &Bson) -> Option<&str> {
    match v {
        Bson::String(s) if s.starts_with('$') && !s.starts_with("$$") => Some(&s[1..]),
        _ => None,
    }
}

/// Checks the parts of `call` that name fields against the rules of the namespaces it touches. Scope and system
/// namespaces are the policy's.
pub fn check(call: &Call, rules: &Rules) -> Res {
    if let Call::RunCommand { db, command, .. } = call {
        return G::new(rules, db, "").command(command);
    }
    let Some(ns) = call.ns() else { return Ok(()) };
    let g = G::new(rules, &ns.db, &ns.coll);
    match call {
        Call::Find { filter, projection, sort, .. } => {
            g.filter(filter, "")?;
            projection.as_ref().map_or(Ok(()), |p| g.projection(p))?;
            sort.as_ref().map_or(Ok(()), |s| g.sort(s))
        }
        Call::Count { filter, .. } | Call::Delete { filter, .. } => g.filter(filter, ""),
        Call::Explain { filter, sort, pipeline, .. } => {
            g.filter(filter, "")?;
            sort.as_ref().map_or(Ok(()), |s| g.sort(s))?;
            pipeline.as_ref().map_or(Ok(()), |p| g.pipeline(p, false))
        }
        Call::Aggregate { pipeline, .. } => g.pipeline(pipeline, false),
        Call::AggregateInto { pipeline, .. } => {
            if g.any {
                return refuse("Data rules apply to this collection, and $out or $merge would copy its fields unscrubbed".into());
            }
            g.out_target(pipeline)?;
            g.pipeline(pipeline, true)
        }
        Call::Update { filter, update, .. } => {
            g.filter(filter, "")?;
            match update {
                Update::Doc(d) => g.update_doc(d),
                Update::Pipeline(p) => g.update_pipeline(p),
            }
        }
        Call::Insert { docs, .. } => docs.iter().try_for_each(|d| g.written(d, "")),
        Call::CreateIndex { keys, .. } => keys.keys().try_for_each(|k| g.pred(k)),
        Call::SelectRows { wher, order, .. } => wher.iter().map(|c| &c.col).chain(order.iter().map(|(c, _)| c)).try_for_each(|c| g.column(c)),
        Call::SqlInsertRows { rows, .. } => rows.iter().flat_map(|row| row.keys()).try_for_each(|c| g.column(c)),
        Call::SqlUpdateRows { set, wher, .. } => set.keys().chain(wher.iter().map(|c| &c.col)).try_for_each(|c| g.column(c)),
        Call::SqlDeleteRows { wher, .. } => wher.iter().try_for_each(|c| g.column(&c.col)),
        _ => Ok(()),
    }
}

/// Best effort only: whether SQL text names one of the hidden columns as an identifier. The real rule is that free
/// SQL is refused where Hide rules reach; this catches the obvious case for a message or a second opinion.
#[cfg(test)]
pub fn sql_mentions_hidden(text: &str, d: Dialect, hidden_cols: &[String]) -> bool {
    tokens(text, d).into_iter().any(|(t, r)| {
        let w = &text[r];
        let name = match t {
            Tok::Word => w,
            Tok::Quoted => w.trim_matches('`').trim_matches('"'),
            Tok::Str if w.starts_with('"') => w.trim_matches('"'),
            _ => return false,
        };
        hidden_cols.iter().any(|h| h.eq_ignore_ascii_case(name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::{Action, Matcher, PresetMode, PresetOverride, Presets, RuleSpec};
    use crate::agents::tools::parse;
    use mongodb::bson::doc;
    use serde_json::{Value, json};

    fn spec(m: Matcher, scope: &[&str], mode: PresetMode) -> RuleSpec {
        RuleSpec { on: true, matcher: m, scope: scope.iter().map(|s| s.to_string()).collect(), action: Action::Remove, mode }
    }

    /// Custom rules on shop.users only: hidden `ssn`, `customer.ssn`, `items.secret` and any `pin`; masked any
    /// `email` and `customer.phone`. shop.orders and shop.products have no rules.
    fn rules() -> Rules {
        let hide = |p: &str| spec(Matcher::FieldPath(p.into()), &["shop.users"], PresetMode::Hide);
        let custom = [
            hide("ssn"),
            hide("customer.ssn"),
            hide("items.secret"),
            spec(Matcher::FieldName("^pin$".into()), &["shop.users"], PresetMode::Hide),
            spec(Matcher::FieldName("^email$".into()), &["shop.users"], PresetMode::MaskOutput),
            spec(Matcher::FieldPath("customer.phone".into()), &["shop.users"], PresetMode::MaskOutput),
        ];
        Rules::compile(&crate::agents::scrub::tests_support::no_presets(), &PresetOverride::default(), &custom, &[], &[1; 32]).unwrap()
    }

    /// The shipped presets: Secrets hidden, PII masked, everywhere.
    fn presets() -> Rules {
        Rules::compile(&Presets::default(), &PresetOverride::default(), &[], &[], &[1; 32]).unwrap()
    }

    fn call(tool: &str, coll: &str, extra: Value) -> Call {
        let mut a = json!({ "connection": 1, "database": "shop", "collection": coll });
        a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        parse(tool, &a).unwrap()
    }

    fn find(filter: Value) -> Res {
        check(&call("find", "users", json!({ "filter": filter })), &rules())
    }

    fn agg(coll: &str, p: Value) -> Res {
        check(&call("aggregate", coll, json!({ "pipeline": p })), &rules())
    }

    fn refused(r: Res) {
        assert!(r.is_err(), "should be refused");
    }

    fn allowed(r: Res) {
        assert!(r.is_ok(), "should be allowed: {r:?}");
    }

    #[test]
    fn probes_on_hidden_fields_are_refused() {
        refused(find(json!({ "ssn": { "$regex": "^1" } })));
        refused(find(json!({ "ssn": { "$gt": "5" } })));
        refused(find(json!({ "ssn": "123-45-6789" })));
        refused(find(json!({ "ssn": { "$exists": true } })));
        refused(find(json!({ "ssn": { "$type": "string" } })));
        refused(find(json!({ "ssn": { "$size": 2 } })));
        refused(find(json!({ "ssn": { "$not": { "$regex": "^1" } } })));
        refused(find(json!({ "$or": [{ "name": "a" }, { "ssn": { "$in": ["1"] } }] })));
        refused(find(json!({ "a": { "pin": 1 } })));
        refused(find(json!({ "x.pin": { "$gt": 1 } })));
        refused(find(json!({ "$where": "this.ssn[0] == '1'" })));
        refused(find(json!({ "$expr": { "$gt": ["$ssn", "5"] } })));
        refused(find(json!({ "$expr": { "$eq": [{ "$size": { "$objectToArray": "$$ROOT" } }, 3] } })));
        refused(find(json!({ "$expr": { "$eq": ["$$ROOT.ssn", "1"] } })));
        refused(find(json!({ "$jsonSchema": { "properties": { "ssn": { "pattern": "^1" } } } })));
        refused(find(json!({ "$text": { "$search": "123" } })));
        refused(find(json!({ "name": { "$unknownOp": 1 } })));
    }

    #[test]
    fn masked_fields_take_any_predicate() {
        allowed(find(json!({ "email": "ana@example.org" })));
        allowed(find(json!({ "email": { "$regex": "@example\\.org$" }, "customer.phone": { "$exists": true } })));
        allowed(find(json!({ "$expr": { "$eq": [{ "$toLower": "$email" }, "ana@example.org"] } })));
        allowed(check(&call("find", "users", json!({ "sort": { "email": 1 }, "projection": { "email": 1, "customer": 1 } })), &rules()));
        allowed(check(&call("count", "users", json!({ "filter": { "email": { "$in": ["a@b.co"] } } })), &rules()));
        allowed(agg("users", json!([{ "$match": { "email": "a@b.co" } }, { "$sort": { "email": -1 } }, { "$unwind": "$email" }, { "$project": { "email": 1 } }])));
    }

    #[test]
    fn masked_fields_cannot_be_copied() {
        refused(agg("users", json!([{ "$project": { "e": "$email" } }])));
        refused(agg("users", json!([{ "$project": { "e": { "$concat": ["$email", ""] } } }])));
        refused(agg("users", json!([{ "$addFields": { "p": "$customer.phone" } }])));
        refused(agg("users", json!([{ "$set": { "nick": "$email" } }])));
        refused(agg("users", json!([{ "$group": { "_id": "$email" } }])));
        refused(agg("users", json!([{ "$group": { "_id": null, "all": { "$push": "$email" } } }])));
        refused(agg("users", json!([{ "$sortByCount": "$email" }])));
        refused(agg("users", json!([{ "$bucket": { "groupBy": "$email", "boundaries": ["a", "z"] } }])));
        refused(agg("users", json!([{ "$replaceRoot": { "newRoot": "$customer" } }])));
        refused(agg("users", json!([{ "$replaceWith": { "x": "$email" } }])));
        refused(agg("users", json!([{ "$facet": { "a": [{ "$project": { "e": "$email" } }] } }])));
        refused(agg("users", json!([{ "$project": { "x": { "$map": { "input": "$emails", "as": "m", "in": "$$m.email" } } } }])));
        let up = |u: Value| check(&call("update", "users", json!({ "filter": {}, "update": u })), &rules());
        refused(up(json!([{ "$set": { "nick": "$email" } }])));
        refused(up(json!({ "$rename": { "email": "contact" } })));
        let distinct = check(&Call::RunCommand { conn: crate::agents::tools::ConnRef::Id(1), db: "shop".into(), command: doc! { "distinct": "users", "key": "email" } }, &rules());
        refused(distinct);
    }

    #[test]
    fn out_and_merge_refuse_a_target_with_data_rules() {
        let into = |p: Value| check(&call("aggregate_into", "orders", json!({ "pipeline": p })), &rules());
        refused(into(json!([{ "$merge": { "into": "users", "on": "_id", "whenMatched": [{ "$set": { "leaked": "$ssn" } }], "whenNotMatched": "discard" } }])));
        refused(into(json!([{ "$out": "users" }])));
        refused(into(json!([{ "$merge": "users" }])));
        allowed(into(json!([{ "$out": "orders_copy" }])));
    }

    #[test]
    fn views_of_a_namespace_with_data_rules_are_refused() {
        let cmd = |c: mongodb::bson::Document| check(&Call::RunCommand { conn: crate::agents::tools::ConnRef::Id(1), db: "shop".into(), command: c }, &rules());
        refused(cmd(doc! { "create": "v", "viewOn": "users", "pipeline": [] }));
        refused(cmd(doc! { "collMod": "v", "viewOn": "users", "pipeline": [] }));
        refused(cmd(doc! { "aggregate": "orders", "pipeline": [{ "$merge": { "into": "users" } }], "cursor": {} }));
        allowed(cmd(doc! { "create": "v", "viewOn": "orders", "pipeline": [] }));
    }

    #[test]
    fn masked_fields_take_literal_writes() {
        let up = |u: Value| check(&call("update", "users", json!({ "filter": { "email": "a@b.co" }, "update": u })), &rules());
        allowed(up(json!({ "$set": { "email": "new@b.co", "customer.phone": "+33 6 12 34 56 78" } })));
        allowed(up(json!({ "$rename": { "nick": "email" } })));
        allowed(check(&call("insert", "users", json!({ "documents": [{ "name": "a", "email": "a@b.co" }] })), &rules()));
        refused(check(&call("insert", "users", json!({ "documents": [{ "name": "a", "pin": 1234 }] })), &rules()));
        refused(up(json!({ "$set": { "ssn": "1" } })));
    }

    #[test]
    fn ancestors_and_array_paths() {
        refused(find(json!({ "customer": { "ssn": "1" } })));
        refused(find(json!({ "customer": { "$gt": {} } })));
        refused(find(json!({ "customer.ssn": { "$gt": "5" } })));
        refused(find(json!({ "items.0.secret": "x" })));
        refused(find(json!({ "items": { "$elemMatch": { "secret": "x" } } })));
        refused(find(json!({ "items.$[].secret": "x" })));
        allowed(find(json!({ "customer": { "$exists": true } })));
        allowed(find(json!({ "customer.name": "a", "items": { "$elemMatch": { "n": { "$gt": 1 } } } })));
        allowed(find(json!({ "items.0.n": 1, "name": { "$regex": "^a" }, "$expr": { "$gt": ["$age", 3] } })));
    }

    #[test]
    fn sorts_and_projections() {
        let f = |extra: Value| check(&call("find", "users", extra), &rules());
        refused(f(json!({ "sort": { "ssn": 1 } })));
        refused(f(json!({ "sort": { "customer": 1 } })));
        refused(f(json!({ "projection": { "x": { "$concat": ["$ssn", ""] } } })));
        refused(f(json!({ "projection": { "x": { "$toString": "$customer" } } })));
        refused(f(json!({ "projection": { "x": "$$ROOT" } })));
        refused(f(json!({ "projection": { "x": { "$getField": "ssn" } } })));
        refused(f(json!({ "projection": { "x": { "$getField": "email" } } })));
        refused(f(json!({ "projection": { "x": { "$getField": { "field": { "$concat": ["s", "sn"] }, "input": "$$ROOT" } } } })));
        refused(f(json!({ "projection": { "items": { "$elemMatch": { "secret": "x" } } } })));
        allowed(f(json!({ "sort": { "name": -1 }, "projection": { "ssn": 0, "customer": 1, "x": { "$toUpper": "$name" }, "y": { "$getField": "name" } } })));
    }

    #[test]
    fn hidden_fields_in_pipelines() {
        refused(agg("users", json!([{ "$project": { "x": { "$concat": ["$ssn"] } } }])));
        refused(agg("users", json!([{ "$project": { "x": "$customer" } }])));
        refused(agg("users", json!([{ "$group": { "_id": "$ssn" } }])));
        refused(agg("users", json!([{ "$group": { "_id": null, "m": { "$max": "$customer" } } }])));
        refused(agg("users", json!([{ "$match": { "ssn": { "$regex": "^1" } } }])));
        refused(agg("users", json!([{ "$sort": { "ssn": 1 } }])));
        refused(agg("users", json!([{ "$unwind": "$ssn" }])));
        refused(agg("users", json!([{ "$redact": "$$KEEP" }])));
        refused(agg("users", json!([{ "$setWindowFields": { "output": {} } }])));
    }

    #[test]
    fn free_pipelines_with_visible_fields() {
        let ok = json!([
            { "$match": { "age": { "$gte": 18 }, "email": { "$exists": true } } },
            { "$addFields": { "double": { "$multiply": ["$age", 2] }, "city": { "$toUpper": "$address.city" } } },
            { "$unwind": { "path": "$items", "preserveNullAndEmptyArrays": true } },
            { "$project": { "name": 1, "city": 1, "customer": 1, "double": 1 } },
            { "$group": { "_id": { "c": "$city" }, "n": { "$count": {} }, "total": { "$sum": "$age" }, "one": { "$sum": 1 }, "names": { "$push": "$name" } } },
            { "$sort": { "n": -1 } }, { "$skip": 1 }, { "$limit": 5 }, { "$unset": ["x"] }, { "$count": "rows" },
        ]);
        allowed(agg("users", ok));
        let r = presets();
        let with_presets = |p: Value| check(&call("aggregate", "orders", json!({ "pipeline": p })), &r);
        allowed(with_presets(json!([{ "$addFields": { "total2": { "$multiply": ["$total", 2] } } }, { "$group": { "_id": "$status", "n": { "$sum": 1 }, "sum": { "$sum": "$total" } } }])));
        allowed(with_presets(json!([{ "$lookup": { "from": "customers", "localField": "email", "foreignField": "email", "as": "c" } }])));
        refused(with_presets(json!([{ "$project": { "k": { "$objectToArray": "$$ROOT" } } }])));
        refused(with_presets(json!([{ "$group": { "_id": "$email" } }])));
        refused(with_presets(json!([{ "$match": { "password": "x" } }])));
        refused(with_presets(json!([{ "$addFields": { "r": "$$ROOT" } }])));
    }

    #[test]
    fn presets_hide_secrets_and_mask_personal_data() {
        let r = presets();
        let f = |filter: Value, sort: Value| check(&call("find", "orders", json!({ "filter": filter, "sort": sort })), &r);
        allowed(f(json!({ "email": "x@y.co" }), json!({ "email": 1 })));
        allowed(f(json!({ "customer.firstName": { "$regex": "^An" } }), json!({ "customer.lastName": 1 })));
        refused(f(json!({ "password": { "$regex": "^a" } }), json!({})));
        refused(f(json!({}), json!({ "apiKey": 1 })));
        allowed(check(&call("insert", "orders", json!({ "documents": [{ "email": "a@b.co", "phone": "+33 6 12 34 56 78" }] })), &r));
        refused(check(&call("insert", "orders", json!({ "documents": [{ "email": "a@b.co", "password": "x" }] })), &r));
        refused(check(&call("aggregate_into", "orders", json!({ "pipeline": [{ "$out": "copy" }] })), &r));
    }

    #[test]
    fn joins() {
        allowed(agg("orders", json!([{ "$addFields": { "r": "$$ROOT" } }, { "$project": { "k": { "$objectToArray": "$$ROOT" } } }, { "$lookup": { "from": "products", "localField": "p", "foreignField": "_id", "as": "p" } }])));
        refused(agg("orders", json!([{ "$lookup": { "from": "users", "localField": "u", "foreignField": "_id", "as": "u" } }])));
        refused(agg("orders", json!([{ "$unionWith": "users" }])));
        refused(agg("orders", json!([{ "$graphLookup": { "from": "users", "startWith": "$u", "connectFromField": "a", "connectToField": "b", "as": "g" } }])));
        refused(agg("orders", json!([{ "$facet": { "a": [{ "$lookup": { "from": "users", "pipeline": [], "as": "u" } }] } }])));
        refused(agg("orders", json!([{ "$out": "copy" }])));
        refused(agg("orders", json!([{ "$merge": { "into": "copy" } }])));
        refused(agg("orders", json!([{ "$currentOp": {} }])));
        refused(agg("orders", json!([{ "$facet": { "a": [{ "$out": "x" }] } }])));
        let r = presets();
        let lookup = |p: Value| check(&call("aggregate", "orders", json!({ "pipeline": p })), &r);
        refused(lookup(json!([{ "$lookup": { "from": "customers", "localField": "password", "foreignField": "x", "as": "c" } }])));
        refused(lookup(json!([{ "$lookup": { "from": "customers", "pipeline": [{ "$project": { "e": "$email" } }], "as": "c" } }])));
        let into = |coll: &str, p: Value| check(&call("aggregate_into", coll, json!({ "pipeline": p })), &rules());
        allowed(into("orders", json!([{ "$match": {} }, { "$out": "copy" }])));
        refused(into("users", json!([{ "$project": { "name": 1 } }, { "$out": "copy" }])));
        refused(into("orders", json!([{ "$out": "a" }, { "$out": "b" }])));
    }

    #[test]
    fn updates_on_hidden_fields() {
        let up = |filter: Value, update: Value| check(&call("update", "users", json!({ "filter": filter, "update": update })), &rules());
        refused(up(json!({}), json!([{ "$set": { "nick": "$ssn" } }])));
        refused(up(json!({}), json!([{ "$set": { "nick": { "$concat": ["$customer.ssn", ""] } } }])));
        refused(up(json!({}), json!([{ "$set": { "ssn": "x" } }])));
        refused(up(json!({}), json!([{ "$replaceWith": "$$ROOT" }])));
        refused(up(json!({}), json!({ "$rename": { "ssn": "nick" } })));
        refused(up(json!({}), json!({ "$rename": { "nick": "ssn" } })));
        refused(up(json!({}), json!({ "$set": { "customer": { "name": "a" } } })));
        refused(up(json!({}), json!({ "$max": { "ssn": "5" } })));
        refused(up(json!({}), json!({ "$set": { "a": { "pin": 1 } } })));
        refused(up(json!({ "ssn": { "$gt": "5" } }), json!({ "$set": { "flag": true } })));
        allowed(up(json!({ "name": "a" }), json!({ "$set": { "nick": "b", "customer.name": "c" }, "$inc": { "n": 1 } })));
        allowed(up(json!({}), json!([{ "$set": { "nick": { "$toUpper": "$name" } } }])));
        refused(check(&call("delete", "users", json!({ "filter": { "ssn": { "$regex": "^1" } } })), &rules()));
        refused(check(&call("insert", "users", json!({ "documents": [{ "name": "a", "customer": { "ssn": "1" } }] })), &rules()));
        allowed(check(&call("insert", "users", json!({ "documents": [{ "name": "a", "customer": { "name": "b" } }] })), &rules()));
        allowed(check(&call("update", "orders", json!({ "filter": { "ssn": "1" }, "update": [{ "$set": { "nick": "$ssn" } }] })), &rules()));
    }

    #[test]
    fn run_command_goes_through_the_guard() {
        let cmd = |c: Document| check(&Call::RunCommand { conn: crate::agents::tools::ConnRef::Id(1), db: "shop".into(), command: c }, &rules());
        allowed(cmd(doc! { "find": "users", "filter": { "email": "a@b.co" }, "sort": { "email": 1 } }));
        refused(cmd(doc! { "find": "users", "filter": { "ssn": "1" } }));
        refused(cmd(doc! { "aggregate": "users", "pipeline": [{ "$group": { "_id": "$email" } }], "cursor": {} }));
        refused(cmd(doc! { "count": "users", "query": { "pin": 1 } }));
        allowed(cmd(doc! { "distinct": "users", "key": "name" }));
        refused(cmd(doc! { "explain": { "find": "users", "filter": { "ssn": "1" } } }));
        allowed(cmd(doc! { "ping": 1 }));
        refused(cmd(doc! { "eval": "db.users.find()" }));
        refused(cmd(doc! { "mapReduce": "users" }));
    }

    #[test]
    fn sql_structured_tools() {
        let custom = [spec(Matcher::FieldName("(?i)^ssn$".into()), &["hr.people"], PresetMode::Hide), spec(Matcher::FieldName("(?i)^email$".into()), &["hr.people"], PresetMode::MaskOutput)];
        let rules = Rules::compile(&crate::agents::scrub::tests_support::no_presets(), &PresetOverride::default(), &custom, &[], &[1; 32]).unwrap();
        let t = |tool: &str, extra: Value| {
            let mut a = json!({ "connection": 1, "database": "hr", "table": "people" });
            a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            check(&parse(tool, &a).unwrap(), &rules)
        };
        refused(t("select_rows", json!({ "where": [{ "column": "SSN", "op": "like", "value": "1%" }] })));
        refused(t("select_rows", json!({ "order": [{ "column": "ssn" }] })));
        refused(t("sql_update_rows", json!({ "set": { "ssn": "x" }, "where": [{ "column": "id", "value": 1 }] })));
        refused(t("sql_insert_rows", json!({ "rows": [{ "id": 1, "ssn": "x" }] })));
        refused(t("sql_delete_rows", json!({ "where": [{ "column": "ssn", "value": "1" }] })));
        allowed(t("select_rows", json!({ "columns": ["id", "ssn"], "where": [{ "column": "email", "op": "like", "value": "%@b.co" }], "order": [{ "column": "email" }] })));
        allowed(t("sql_update_rows", json!({ "set": { "email": "x@b.co" }, "where": [{ "column": "email", "value": "a@b.co" }] })));
    }

    #[test]
    fn sql_text_mentions() {
        let h = vec!["ssn".to_string()];
        assert!(sql_mentions_hidden("SELECT upper(ssn) FROM people", Dialect::Postgres, &h));
        assert!(sql_mentions_hidden("SELECT `SSN` FROM people", Dialect::MySql, &h));
        assert!(sql_mentions_hidden(r#"SELECT "ssn" FROM people"#, Dialect::Postgres, &h));
        assert!(!sql_mentions_hidden("SELECT 'ssn', name FROM people -- ssn", Dialect::MySql, &h));
        assert!(!sql_mentions_hidden("SELECT row_to_json(p) FROM people p", Dialect::Postgres, &h), "why free SQL is refused outright where Hide rules reach");
    }

    #[test]
    fn calls_without_a_namespace_pass_through() {
        allowed(check(&Call::ListConnections, &rules()));
    }
}
