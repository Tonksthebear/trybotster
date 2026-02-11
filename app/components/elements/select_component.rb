module Elements
  class SelectComponent < ApplicationComponent
    attr_reader :options

    renders_one :button, ->(prompt: nil, **options) do
      prompt ||= @prompt
      ButtonComponent.new(prompt: prompt, variant: @variant, **options)
    end
    renders_one :menu, ->(**options) do
      MenuComponent.new(variant: @variant, anchor: @anchor, **options)
    end

    def initialize(variant: :default, prompt: nil, anchor: "bottom", include_blank: nil, **options)
      @prompt = prompt
      @variant = variant
      @options = options
      @anchor = anchor
      @include_blank = include_blank
      @required = options[:required]
    end

    def option(value:, display: nil, **options, &block)
      content = block ? capture(&block) : (display || value)
      base_classes = yass(select: { @variant => :option })
      options[:class] = class_names(options[:class], base_classes)

      tag.el_option(content,
        value: value,
        **options
      )
    end

    def before_render
      blank_text = determine_blank_text
      if blank_text
        menu.prepended_menu_content = option(value: "", display: blank_text)
      end
    end

    private

    def determine_blank_text
      # Rails raises error for this combination
      if @required && @include_blank == false
        raise ArgumentError, "include_blank cannot be false for a required field"
      end

      # Explicit include_blank takes precedence
      if @include_blank
        return @include_blank == true ? "" : @include_blank
      end

      # Prompt only shows when no value selected
      if @prompt && value_blank?
        return @prompt == true ? "Please select" : @prompt
      end

      # Required fields auto-add blank for HTML5 validation
      if @required
        return ""
      end

      # Default: no blank
      nil
    end

    def value_blank?
      @options[:value].blank?
    end

    public

    def default_button
      render ButtonComponent.new(variant: @variant, prompt: @prompt) do |button|
        button.selected_content
      end
    end

    def default_menu
      MenuComponent.new(variant: @variant).with_content(content || "")
    end

    class MenuComponent < ApplicationComponent
      attr_reader :anchor, :options, :prepended_content

      def initialize(anchor: "bottom", variant:, **options)
        @anchor = anchor
        @options = options
        @variant = variant
      end

      def prepended_menu_content=(prepended_content)
        @prepended_content = prepended_content
      end

      def before_render
        @options[:class] = class_names(@options[:class], anchor_to_origin_class(anchor), yass(select: { @variant => :menu }))
      end
    end

    class ButtonComponent < ApplicationComponent
      attr_reader :options, :called_selected_content

      def initialize(prompt: nil, variant:, **options)
        @prompt = prompt || "Choose one"
        @options = options
        @options[:type] = :button
        @variant = variant
        @called_selected_content = false
      end

      def before_render
        @options[:class] = class_names(@options[:class], yass(select: { @variant => :button }))
      end

      def selected_content(prompt = @prompt, **options)
        @called_selected_content = true
        options[:class] ||= yass(select: { @variant => :selected })
        tag.el_selectedcontent(prompt, **options)
      end
    end
  end
end
