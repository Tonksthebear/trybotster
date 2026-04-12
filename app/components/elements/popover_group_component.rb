module Elements
  class PopoverGroupComponent < ApplicationComponent
    attr_reader :options

    def initialize(**options)
      @options = options
      @button_index = 0
      @popover_index = 0
    end

    def button(text = nil, id: nil, **options, &block)
      @button_index += 1
      id ||= "#{id_prefix}-#{@button_index}"
      options[:popovertarget] = id
      render Elements::ButtonComponent.new(text, unstyled: true, **options, &block)
    end

    def popover(id: nil, anchor: "bottom", style: :anchored, **options, &block)
      @popover_index += 1
      id ||= "#{id_prefix}-#{@popover_index}"
      pc = Elements::PopoverComponent.new(id: id, anchor: anchor, style: style)
      render(pc) { pc.popover(**options, &block) }
    end

    def call
      tag.el_popover_group(content, **options)
    end

    private

    def id_prefix
      @id_prefix ||= "popover-group-#{SecureRandom.hex(4)}"
    end
  end
end
