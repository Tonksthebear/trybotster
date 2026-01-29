require "turbo_stream_builder_overrides"

Turbo::Streams::TagBuilder.prepend(TurboStreamBuilderOverrides)
