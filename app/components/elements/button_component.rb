module Elements
  class ButtonComponent < ApplicationComponent
    attr_reader :as, :args, :classes

    def initialize(text_content = "", as: :button, color: :primary, variant: :solid, shape: :base, size: :md, unstyled: false, **args, &block)
      @text_content = text_content
      @as = as
      @unstyled = unstyled
      @color = color
      @variant = variant
      @shape = shape
      @size = size
      @user_class = args.delete(:class)
      @args = args
      @block = block
    end

    def before_render
      base = @unstyled ? nil : yass(btn: [ :base, { @size => @shape }, { @color => @variant } ])
      @classes = class_names(base, process_user_class(@user_class))
    end

    def call
      tag.send(as, content || @text_content, **args, class: classes, &@block)
    end

    private

    def process_user_class(value)
      value.is_a?(Hash) ? yass(value) : value
    end
  end
end
