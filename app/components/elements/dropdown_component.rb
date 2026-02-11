module Elements
  class DropdownComponent < ApplicationComponent
    attr_reader :anchor, :options
    renders_one :button, ->(**options, &block) do
      Elements::ButtonComponent.new(unstyled: true, **options, &block)
    end

    def initialize(anchor: "bottom end", panel: nil, animation: nil, spacing: nil, **options)
      @anchor = anchor
      @panel = panel
      @animation = animation
      @spacing = spacing
      @options = options
    end

    def before_render
      # Only add inline-block if no block-level display class was passed
      unless @options[:class].to_s.match?(/\bblock\b/)
        @options[:class] = class_names(@options[:class], "inline-block")
      end
    end

    def menu_classes
      [
        @panel || yass(dropdown: { menu: :panel }, add: "transition transition-discrete"),
        @animation || yass(dropdown: { menu: :animation }),
        @spacing || yass(dropdown: { menu: :spacing }),
        origin_class
      ].join(" ")
    end

    # We need to explicitly list the full class for tailwind to generate the correct classes
    def origin_class
      anchor_to_origin_class(@anchor)
    end
  end
end
