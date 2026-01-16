module Elements
  class FormBuilder < ActionView::Helpers::FormBuilder
    # Main select method - mimics Rails form.select API
    def elements_select(method, choices = nil, options = {}, html_options = {}, &block)
      # Get the current value from the model
      selected_value = @object&.public_send(method)

      # Merge form field attributes (don't override existing styling)
      field_options = {
        name: field_name(method),
        id: field_id(method),
        value: selected_value || ""
      }

      # Add prompt if provided in options
      field_options[:prompt] = options[:prompt] if options[:prompt]

      # Add include_blank if provided in options
      field_options[:include_blank] = options[:include_blank] if options.key?(:include_blank)

      # Add required if provided in options
      field_options[:required] = options[:required] if options[:required]

      # Merge with html_options (html_options take precedence)
      field_options.merge!(html_options)

      # If choices provided, create a simple select with those options
      if choices.present? && block.nil?
        @template.render ::Elements::SelectComponent.new(**field_options) do |select|
          select.with_menu do
            choices.map do |choice|
              case choice
              when Array
                # Handle [["Display", "value"], ...] format
                display, value = choice
                select.option(value: value, display: display)
              when Hash
                # Handle [{text: "Display", value: "value"}, ...] format
                select.option(value: choice[:value], display: choice[:text] || choice[:display])
              else
                # Handle simple array ["option1", "option2", ...]
                select.option(value: choice)
              end
            end.join.html_safe
          end
        end
      else
        # Custom block usage - full flexibility
        @template.render ::Elements::SelectComponent.new(**field_options), &block
      end
    end

    # Toggle method - mimics Rails form.checkbox API but renders through ToggleComponent
    def toggle(method, options = {}, checked_value = "1", unchecked_value = "0")
      # Get the current value from the model
      current_value = @object&.public_send(method)

      # Determine if the toggle should be checked
      is_checked = case current_value
      when checked_value, true, "true", 1, "1"
                     true
      else
                     false
      end

      # Merge form field attributes
      field_options = {
        name: field_name(method),
        id: field_id(method),
        value: checked_value,
        checked: is_checked
      }

      # Add hidden field for unchecked value (Rails convention)
      hidden_field_tag = @template.tag.input(
        type: "hidden",
        name: field_name(method),
        value: unchecked_value,
        autocomplete: "off"
      )

      # Merge with provided options (options take precedence)
      field_options.merge!(options)

      # Render the component with hidden field
      hidden_field_tag + @template.render(::Elements::ToggleComponent.new(field_name(method), **field_options))
    end

    # Alias for singular form
    alias_method :element_select, :elements_select
  end
end
