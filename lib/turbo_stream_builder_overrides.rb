
module TurboStreamBuilderOverrides
  def update_attribute(target, attribute, content = nil, method: nil, **rendering, &block)
    action :update_attribute, target, content, method: method, stream_attributes: { attribute: attribute }, **rendering, &block
  end

  def action(name, target, content = nil, method: nil, allow_inferred_rendering: true, stream_attributes: {}, **rendering, &block)
    template = render_template(target, content, allow_inferred_rendering: allow_inferred_rendering, **rendering, &block)

    turbo_stream_action_tag name, target: target, template: template, method: method, **stream_attributes
  end
end
