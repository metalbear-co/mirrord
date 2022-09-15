use spinoff::{Color, Spinner, Spinners};
use tracing::{field::Visit, span, subscriber::Interest, Subscriber};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

/// The environment variable name that is used
/// to determine the mode of progress reporting
pub const MIRRORD_PROGRESS_ENV: &str = "MIRRORD_ENABLE_PROGRESS";

/// `ProgressMode` specifies the way progress is reported
/// by [`PrintProgress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    /// Display dynamic progress with spinners and colors.
    Standard,
    /// Display simple human-readable messages in new lines.
    Simple,
    /// Output progress messages in JSON format for programmatic use.
    Json,
    /// Do not output progress.
    Off,
}

/// A [`tracing_subscriber::Layer`] that records progress
/// via spans and reports it to the standard output.
///
/// It looks for the following span fields:
///
/// - `term_progress`: the message to display when the span is active.
/// - `term_done`: the message to display when the span has been closed.
///
/// All fields must be `&'static str`, any other types are ignored.
///
/// If a span does not contain the field `term_progress`, it is ignored.
pub struct PrintProgress {
    mode: ProgressMode,
}

impl PrintProgress {
    pub fn new(mode: ProgressMode) -> Self {
        Self { mode }
    }

    pub fn from_env(fallback: ProgressMode) -> Self {
        Self::new(match std::env::var(MIRRORD_PROGRESS_ENV).as_deref() {
            Ok("std" | "standard") => {
                if atty::is(atty::Stream::Stdout) {
                    ProgressMode::Standard
                } else {
                    ProgressMode::Simple
                }
            }
            Ok("dumb" | "simple") => ProgressMode::Simple,
            Ok("json") => ProgressMode::Json,
            Ok("off") => ProgressMode::Off,
            _ => fallback,
        })
    }
}

impl<S> Layer<S> for PrintProgress
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        if matches!(self.mode, ProgressMode::Off) {
            false
        } else {
            metadata
                .fields()
                .iter()
                .any(|f| f.name() == "term_progress")
        }
    }

    fn register_callsite(
        &self,
        metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        if metadata.level() == &tracing::Level::INFO && metadata.is_span() {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        #[derive(Default)]
        struct FieldVisitor {
            term_progress: Option<String>,
            term_done: Option<String>,
        }

        impl Visit for FieldVisitor {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                match field.name() {
                    "term_progress" => {
                        self.term_progress = Some(value.to_string());
                    }
                    "term_done" => {
                        self.term_done = Some(value.to_string());
                    }
                    _ => {}
                }
            }

            fn record_debug(
                &mut self,
                _field: &tracing::field::Field,
                _value: &dyn std::fmt::Debug,
            ) {
            }
        }

        let mut visitor = FieldVisitor::default();
        attrs.values().record(&mut visitor);

        let span = ctx.span(id).unwrap();
        let mut ext = span.extensions_mut();

        if let Some(progress) = visitor.term_progress {
            ext.insert(ext::ProgressMessage(progress));
        }

        if let Some(done) = visitor.term_done {
            ext.insert(ext::ProgressDoneMessage(done));
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).unwrap();

        if self.mode == ProgressMode::Simple {
            let ext = span.extensions();
            if let Some(ext::ProgressMessage(msg)) = ext.get() {
                println!("{msg}");
            }
            return;
        }

        {
            let ext = span.extensions();
            if ext.get::<ext::Entered>().is_some() {
                return;
            }
        }

        {
            let mut ext = span.extensions_mut();
            ext.insert(ext::Entered);
        }

        let ext = span.extensions();

        if let Some(ext::ProgressMessage(progress_message)) = ext.get() {
            let progress_message = progress_message.clone();
            drop(ext);
            let parent_with_progress = span
                .scope()
                .find(|sp| sp.extensions().get::<Spinner>().is_some());

            if let Some(parent) = parent_with_progress {
                let mut ext = parent.extensions_mut();
                let spinner = ext.get_mut::<Spinner>().unwrap();
                spinner.update_text(progress_message);
            } else {
                span.extensions_mut().insert(Spinner::new(
                    Spinners::Dots,
                    progress_message,
                    Color::White,
                ));
            }
        }
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).unwrap();

        if self.mode == ProgressMode::Simple {
            let ext = span.extensions();
            if let Some(ext::ProgressDoneMessage(msg)) = ext.get() {
                println!("{msg}");
            }
            return;
        }

        let mut ext = span.extensions_mut();
        if let Some(spinner) = ext.remove::<Spinner>() {
            drop(ext);
            let ext = span.extensions();
            let done_message = ext
                .get::<ext::ProgressDoneMessage>()
                .map(|m| m.0.as_str())
                .or_else(|| ext.get::<ext::ProgressMessage>().map(|m| m.0.as_str()));

            if let Some(done_message) = done_message {
                spinner.success(done_message);
            }
            return;
        }

        if ext.get_mut::<ext::ProgressMessage>().is_some() {
            drop(ext);
            let parent_with_progress = span
                .scope()
                .find(|sp| sp.extensions().get::<Spinner>().is_some());

            if let Some(parent) = parent_with_progress {
                let parent_ext = parent.extensions();
                let parent_progress_message = parent_ext
                    .get::<ext::ProgressMessage>()
                    .map(|m| m.0.clone())
                    .unwrap();

                drop(parent_ext);
                let mut parent_ext = parent.extensions_mut();
                let spinner = parent_ext.get_mut::<Spinner>().unwrap();
                spinner.update_text(parent_progress_message);
            }
        }
    }
}

/// Span extensions
mod ext {
    pub(super) struct Entered;

    #[derive(Clone)]
    #[repr(transparent)]
    pub(super) struct ProgressMessage(pub(super) String);
    #[derive(Clone)]
    #[repr(transparent)]
    pub(super) struct ProgressDoneMessage(pub(super) String);
}
