module Elements
  class SidebarComponent < ApplicationComponent
    renders_one :sidebar
    renders_one :desktop_sidebar
    renders_one :mobile_sidebar

    renders_one :header, ->(**options, &block) do
      @header_classes = options[:class] || "bg-white dark:border-white/5 dark:bg-gray-900 lg:flex"
      @header_content = block&.call
    end

    renders_one :mobile_menu_button, ->(**options, &block) do
      @mobile_menu_classes = options[:class]|| "text-gray-600 dark:text-white"
      @mobile_menu_content = block&.call
    end

    attr_reader :options, :header_classes, :mobile_menu_classes, :mobile_menu_content

    def initialize(**options)
      case options[:class]
      when Hash
        options[:class][:add] = "#{options[:class][:add]} lg:pl-(--sidebar-width)"
      when String
        options[:class] = class_names(options[:class], "lg:pl-(--sidebar-width)")
      else
        options[:class] = "lg:pl-(--sidebar-width)"
      end
      @options = options
    end

    # Empty class required for ViewComponent's sidecar template lookup.
    # Template: mobile_component.html.erb contains the mobile dialog markup.
    class MobileComponent < ApplicationComponent
    end

    # Empty class required for ViewComponent's sidecar template lookup.
    # Template: desktop_component.html.erb contains the fixed sidebar markup.
    class DesktopComponent < ApplicationComponent
    end
  end
end
