use collab::preclude::lib0Any;
use collab_database::database::{gen_row_id, DatabaseData};
use collab_database::fields::Field;
use collab_database::rows::CreateRowParams;
use collab_database::views::{CreateViewParams, DatabaseLayout, LayoutSettingBuilder};
use nanoid::nanoid;
use serde_json::json;

use assert_json_diff::{assert_json_eq, assert_json_include};

use crate::database_test::helper::{
  create_database, create_database_with_default_data, default_field_settings_by_layout,
};
use crate::helper::TestFilter;

#[tokio::test]
async fn create_initial_database_test() {
  let database_test = create_database(1, "1").await;
  assert_json_include!(
    expected: json!( {
      "fields": [],
      "inline_view": "v1",
      "rows": [],
      "views": [
        {
          "database_id": "1",
          "field_orders": [],
          "filters": [],
          "group_settings": [],
          "id": "v1",
          "layout": 0,
          "layout_settings": {},
          "name": "my first database view",
          "row_orders": [],
          "sorts": []
        }
      ]
    }),
    actual: database_test.to_json_value()
  );
}

#[tokio::test]
async fn create_database_with_single_view_test() {
  let database_test = create_database_with_default_data(1, "1").await;
  let view = database_test.views.get_view("v1").unwrap();
  assert_eq!(view.row_orders.len(), 3);
  assert_eq!(view.field_orders.len(), 3);
}

#[tokio::test]
async fn get_database_view_description_test() {
  let database_test = create_database_with_default_data(1, "1").await;
  let views = database_test.get_all_views_description();
  assert_eq!(views.len(), 1);
  assert_eq!(views[0].name, "my first database view");
}

#[tokio::test]
async fn create_same_database_view_twice_test() {
  let database_test = create_database_with_default_data(1, "1").await;
  let params = CreateViewParams {
    database_id: "1".to_string(),
    view_id: "v1".to_string(),
    name: "my second grid".to_string(),
    layout: DatabaseLayout::Grid,
    ..Default::default()
  };
  database_test.create_linked_view(params).unwrap();
  let view = database_test.views.get_view("v1").unwrap();

  assert_eq!(view.name, "my second grid");
}

#[tokio::test]
async fn create_database_row_test() {
  let database_test = create_database_with_default_data(1, "1").await;

  let row_id = gen_row_id();
  database_test
    .create_row(CreateRowParams {
      id: row_id.clone(),
      ..Default::default()
    })
    .unwrap();

  let view = database_test.views.get_view("v1").unwrap();
  assert_json_eq!(view.row_orders.last().unwrap().id, row_id);
}

#[tokio::test]
async fn create_database_field_test() {
  let database_test = create_database_with_default_data(1, "1").await;

  let field_id = nanoid!(4);
  database_test.create_field(
    Field {
      id: field_id.clone(),
      name: "my third field".to_string(),
      ..Default::default()
    },
    default_field_settings_by_layout(),
  );

  let view = database_test.views.get_view("v1").unwrap();
  assert_json_eq!(view.field_orders.last().unwrap().id, field_id);
}

#[tokio::test]
async fn create_database_view_with_filter_test() {
  let database_test = create_database_with_default_data(1, "1").await;
  let filter_1 = TestFilter {
    id: "filter1".to_string(),
    field_id: "".to_string(),
    field_type: Default::default(),
    condition: 0,
    content: "".to_string(),
  };

  let filter_2 = TestFilter {
    id: "filter2".to_string(),
    field_id: "".to_string(),
    field_type: Default::default(),
    condition: 0,
    content: "".to_string(),
  };

  let params = CreateViewParams {
    database_id: "1".to_string(),
    view_id: "v1".to_string(),
    name: "my first grid".to_string(),
    filters: vec![filter_1.into(), filter_2.into()],
    layout: DatabaseLayout::Grid,
    ..Default::default()
  };
  database_test.create_linked_view(params).unwrap();

  let view = database_test.views.get_view("v1").unwrap();
  let filters = view
    .filters
    .into_iter()
    .map(|value| TestFilter::try_from(value).unwrap())
    .collect::<Vec<TestFilter>>();
  assert_eq!(filters.len(), 2);
  assert_eq!(filters[0].id, "filter1");
  assert_eq!(filters[1].id, "filter2");
}

#[tokio::test]
async fn create_database_view_with_layout_setting_test() {
  let database_test = create_database_with_default_data(1, "1").await;
  let grid_setting = LayoutSettingBuilder::new()
    .insert_i64_value("1", 123)
    .insert_any("2", "abc")
    .build();

  let params = CreateViewParams {
    database_id: "1".to_string(),
    view_id: "v1".to_string(),
    name: "my first grid".to_string(),
    layout: DatabaseLayout::Grid,
    ..Default::default()
  }
  .with_layout_setting(grid_setting);
  database_test.create_linked_view(params).unwrap();

  let view = database_test.views.get_view("v1").unwrap();
  let grid_layout_setting = view.layout_settings.get(&DatabaseLayout::Grid).unwrap();
  assert_eq!(grid_layout_setting.get("1").unwrap(), &lib0Any::BigInt(123));
  assert_eq!(
    grid_layout_setting.get("2").unwrap(),
    &lib0Any::String("abc".to_string().into_boxed_str())
  );
}

#[tokio::test]
async fn delete_inline_database_view_test() {
  let database_test = create_database_with_default_data(1, "1").await;
  for i in 0..3 {
    let params = CreateViewParams {
      database_id: "1".to_string(),
      view_id: format!("v{}", i),
      ..Default::default()
    };
    database_test.create_linked_view(params).unwrap();
  }

  let views = database_test.views.get_all_views();
  let view_id = views[1].id.clone();
  assert_eq!(views.len(), 3);

  database_test.views.delete_view(&view_id);
  let views = database_test
    .views
    .get_all_views()
    .iter()
    .map(|view| view.id.clone())
    .collect::<Vec<String>>();
  assert_eq!(views.len(), 2);
  assert!(!views.contains(&view_id));
}

#[tokio::test]
async fn duplicate_database_view_test() {
  let database_test = create_database_with_default_data(1, "1").await;
  database_test.duplicate_linked_view("v1");

  let views = database_test.views.get_all_views();
  assert_eq!(views.len(), 2);
}

#[tokio::test]
async fn duplicate_database_data_serde_test() {
  let database_test = create_database_with_default_data(1, "1").await;
  let duplicated_database = database_test.duplicate_database();

  let json = duplicated_database.to_json().unwrap();
  let duplicated_database2 = DatabaseData::from_json(&json).unwrap();
  assert_eq!(
    duplicated_database.fields.len(),
    duplicated_database2.fields.len()
  );
  assert_eq!(
    duplicated_database.rows.len(),
    duplicated_database2.rows.len()
  );
}

#[tokio::test]
async fn get_database_view_layout_test() {
  let database_test = create_database_with_default_data(1, "1").await;

  let layout = database_test.views.get_database_view_layout("v1");
  assert_eq!(layout, DatabaseLayout::Grid);
}

#[tokio::test]
async fn update_database_view_layout_test() {
  let database_test = create_database_with_default_data(1, "1").await;
  database_test.views.update_database_view("v1", |update| {
    update.set_layout_type(DatabaseLayout::Calendar);
  });

  let layout = database_test.views.get_database_view_layout("v1");
  assert_eq!(layout, DatabaseLayout::Calendar);
}
