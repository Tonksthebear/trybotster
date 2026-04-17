# frozen_string_literal: true

require "test_helper"

class DocsControllerTest < ActionDispatch::IntegrationTest
  test "show renders the docs page without the SPA shell" do
    get docs_path

    assert_response :success
    assert_select "article"
    assert_select "summary", text: /Documentation Menu/
    assert_select "#app", false
  end

  test "show redirects invalid docs paths to the first page" do
    get doc_path(path: "not/a-real-page")

    assert_redirected_to doc_path(path: "getting-started/installation")
  end
end
