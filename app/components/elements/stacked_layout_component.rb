module Elements
  class StackedLayoutComponent < ApplicationComponent
    renders_one :navbar
    renders_one :mobile_menu

    renders_one :mobile_menu_button, ->(**options, &block) do
      @mobile_menu_button_classes = options[:class]
      @mobile_menu_button_content = capture(&block) if block
    end

    # Optional header rendered inside the dark zone for overlap style
    renders_one :header

    attr_reader :style, :options, :mobile_menu_button_classes, :mobile_menu_button_content

    def initialize(style: :border, **options)
      @style = style
      @options = options
    end

    def overlap?
      style == :overlap
    end
  end
end
