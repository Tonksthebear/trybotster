module Elements
  class ToggleComponent < ApplicationComponent
    def initialize(name, **options)
      @name = name
      @options = options
    end

    def before_render
      @options[:class] = class_names(@options[:class], yass({ toggle: :input }))
    end

    def call
      content_tag(:div, class: { toggle: :container }) do
        concat tag.span(class: { toggle: :span })
        concat check_box_tag(@name, **@options)
      end
    end
  end
end
