module Elements
  class InputComponent < ApplicationComponent
    renders_one :prepend
    renders_one :append

    attr_reader :attributes

    def initialize(**attributes)
      @attributes = attributes
      if @attributes[:class].is_a?(Hash)
        @attributes[:class][:add] = class_names(@attributes[:class][:add], "group/input")
      else
        @attributes[:class] = class_names(@attributes[:class], "group/input")
      end
    end
  end
end
