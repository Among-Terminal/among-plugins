use super::{
    stdio, wire, Admission, PageConsumer, PageResult, Reference, Scope, MAX_CONTEXT_PAGE_BYTES,
    MAX_FRAME_BYTES, MAX_PENDING_STATUS, MAX_RETAINED_ENCODED_BYTES, TRAVERSAL_BUDGET,
};
use crate::protocol::Status;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::Instant;

struct Pending<S> {
    request_id: u64,
    scope: Scope,
    cursor: Reference,
    state: S,
    deadline: Instant,
    encoded: usize,
}

pub(super) struct Paging<S> {
    pending: VecDeque<Pending<S>>,
    active: Option<u64>,
    next_id: Option<u64>,
    encoded: usize,
    deadline: stdio::Deadline,
}

fn cursor(context: &Value) -> io::Result<Reference> {
    serde_json::from_value(
        context
            .get("next_cursor")
            .cloned()
            .ok_or_else(wire::invalid)?,
    )
    .map_err(|_| wire::invalid())
}

fn measure<S: Serialize>(
    admission: &Admission,
    request_id: u64,
    scope: &Scope,
    cursor: &Reference,
    state: &S,
) -> io::Result<usize> {
    #[derive(Serialize)]
    struct Record<'a, S> {
        session: &'a str,
        generation: &'a str,
        request_id: u64,
        scope: &'a Scope,
        cursor: &'a Reference,
        state: &'a S,
    }
    wire::measure(state, MAX_CONTEXT_PAGE_BYTES)?;
    let record = Record {
        session: &admission.session,
        generation: &admission.generation,
        request_id,
        scope,
        cursor,
        state,
    };
    wire::measure(&record, MAX_FRAME_BYTES - 1)?
        .checked_add(1)
        .ok_or_else(wire::invalid)
}

pub(super) fn reply(
    output: &mut impl Write,
    admission: &Admission,
    request_id: u64,
    status: Status,
) -> io::Result<()> {
    wire::write_frame(
        output,
        &json!({"type":"status","session":admission.session,
        "generation":admission.generation,"request_id":request_id,
        "refresh_ms":status.refresh_ms,"fragments":status.fragments}),
    )
}

impl<S: Copy + Serialize + 'static> Paging<S> {
    pub(super) fn new(deadline: stdio::Deadline) -> io::Result<Self> {
        if std::mem::size_of::<S>() > MAX_CONTEXT_PAGE_BYTES {
            return Err(wire::invalid());
        }
        std::mem::size_of::<Pending<S>>()
            .checked_mul(MAX_PENDING_STATUS)
            .ok_or_else(wire::invalid)?;
        Ok(Self {
            pending: VecDeque::with_capacity(MAX_PENDING_STATUS),
            active: None,
            next_id: Some(1),
            encoded: 0,
            deadline,
        })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn admit(&self, scope: &Scope) -> io::Result<()> {
        stdio::check(&self.deadline)?;
        if self.pending.len() >= MAX_PENDING_STATUS
            || self.pending.iter().any(|pending| &pending.scope == scope)
        {
            return Err(wire::invalid());
        }
        Ok(())
    }

    fn update_deadline(&self) {
        self.deadline
            .set(self.pending.iter().map(|pending| pending.deadline).min());
    }

    pub(super) fn begin(
        &mut self,
        admission: &Admission,
        request_id: u64,
        scope: Scope,
        context: &Value,
        state: S,
        started: Instant,
    ) -> io::Result<()> {
        self.admit(&scope)?;
        if !admission.features.contains("context.terminals") {
            return Err(wire::invalid());
        }
        let cursor = cursor(context)?;
        let deadline = started
            .checked_add(TRAVERSAL_BUDGET)
            .ok_or_else(wire::invalid)?;
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "managed traversal expired",
            ));
        }
        let encoded = measure(admission, request_id, &scope, &cursor, &state)?;
        let total = self
            .encoded
            .checked_add(encoded)
            .filter(|total| *total <= MAX_RETAINED_ENCODED_BYTES)
            .ok_or_else(wire::invalid)?;
        self.pending.push_back(Pending {
            request_id,
            scope,
            cursor,
            state,
            deadline,
            encoded,
        });
        self.encoded = total;
        self.update_deadline();
        stdio::check(&self.deadline)
    }

    pub(super) fn enumerate(
        &mut self,
        output: &mut impl Write,
        admission: &Admission,
    ) -> io::Result<()> {
        stdio::check(&self.deadline)?;
        if self.active.is_some() {
            return Ok(());
        }
        let Some(pending) = self.pending.front() else {
            return Ok(());
        };
        let id = self.next_id.ok_or_else(wire::invalid)?;
        self.next_id = id.checked_add(1);
        wire::write_frame(
            output,
            &json!({"type":"enumerate","session":admission.session,
            "generation":admission.generation,"request_id":id,"scope":pending.scope,"cursor":pending.cursor}),
        )?;
        self.active = Some(id);
        Ok(())
    }

    pub(super) fn page(
        &mut self,
        output: &mut impl Write,
        admission: &Admission,
        request_id: u64,
        scope: &Scope,
        context: &Value,
        consumer: &mut impl PageConsumer<State = S>,
    ) -> io::Result<()> {
        stdio::check(&self.deadline)?;
        let pending = self.pending.front().ok_or_else(wire::invalid)?;
        if self.active != Some(request_id) || &pending.scope != scope {
            return Err(wire::invalid());
        }
        let result = consumer.page(scope, pending.state, context)?;
        stdio::check(&self.deadline)?;
        match result {
            PageResult::Complete(status) => {
                reply(output, admission, pending.request_id, status)?;
                let pending = self.pending.pop_front().ok_or_else(wire::invalid)?;
                self.encoded = self
                    .encoded
                    .checked_sub(pending.encoded)
                    .ok_or_else(wire::invalid)?;
            }
            PageResult::Continue(state) => {
                let cursor = cursor(context)?;
                let encoded = measure(admission, pending.request_id, scope, &cursor, &state)?;
                let total = self
                    .encoded
                    .checked_sub(pending.encoded)
                    .and_then(|total| total.checked_add(encoded))
                    .filter(|total| *total <= MAX_RETAINED_ENCODED_BYTES)
                    .ok_or_else(wire::invalid)?;
                let pending = self.pending.front_mut().ok_or_else(wire::invalid)?;
                pending.cursor = cursor;
                pending.state = state;
                pending.encoded = encoded;
                self.encoded = total;
            }
        }
        self.active = None;
        self.update_deadline();
        self.enumerate(output, admission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Clone, Copy)]
    struct TooLarge([u8; MAX_CONTEXT_PAGE_BYTES + 1]);

    impl Serialize for TooLarge {
        fn serialize<T: serde::Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
            serializer.serialize_u8(self.0[0])
        }
    }

    #[test]
    fn physical_state_is_rejected_before_pending_table_allocation() {
        assert!(Paging::<TooLarge>::new(Rc::new(Cell::new(None))).is_err());
    }

    #[test]
    fn enumeration_ids_never_wrap_or_reuse_the_maximum_value() {
        let admission = Admission {
            session: "s".into(),
            generation: "g".into(),
            features: ["status".into(), "context.terminals".into()].into(),
            last_request: Some(7),
        };
        let scope = Scope {
            slot: "app_header".into(),
            target: super::super::Target::Application,
        };
        let mut paging = Paging::<u64>::new(Rc::new(Cell::new(None))).unwrap();
        paging
            .begin(
                &admission,
                7,
                scope,
                &json!({"next_cursor":"1"}),
                1,
                Instant::now(),
            )
            .unwrap();
        paging.next_id = Some(u64::MAX);
        let mut output = Vec::new();
        paging.enumerate(&mut output, &admission).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap()["request_id"],
            u64::MAX
        );
        let before = output.clone();
        paging.active = None;
        assert!(paging.enumerate(&mut output, &admission).is_err());
        assert_eq!(output, before);
        assert_eq!(admission.last_request, Some(7));
    }
}
