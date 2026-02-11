Classy::Yaml.setup do |config|
  config.extra_files = [ Rails.root.join("config", "elements.yml") ]
  config.override_tag_helpers = true
end
