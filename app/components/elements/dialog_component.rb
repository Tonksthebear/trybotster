module Elements
  class DialogComponent < ApplicationComponent
    attr_reader :id, :open, :style

    def initialize(id:, open: false, style: :centered)
      @id = id
      @open = open
      @style = style
    end
  end
end
