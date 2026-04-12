module Elements
  class PopoverComponent < ApplicationComponent
    self.yaml_root = :popover

    attr_reader :id, :anchor

    def initialize(id: nil, anchor: "bottom", style: :anchored)
      @id = id || "popover-#{SecureRandom.hex(4)}"
      @anchor = anchor
      @style = style
    end

    def call
      content
    end

    def button(text = nil, **options, &block)
      options[:popovertarget] = @id
      render Elements::ButtonComponent.new(text, unstyled: true, **options, &block)
    end

    def popover(**options, &block)
      popover_content = capture(&block)
      options[:class] = class_names(yass(@style), options[:class])

      popover_options = { id: @id, popover: true, **options }
      popover_options[:anchor] = @anchor if @style == :anchored

      tag.el_popover(popover_content, **popover_options)
    end
  end
end
