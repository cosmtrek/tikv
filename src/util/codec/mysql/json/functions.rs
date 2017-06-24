// Copyright 2017 PingCAP, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.

// FIXME: remove following later
#![allow(dead_code)]

use std::{u32, char, str};
use super::super::Result;
use super::Json;
use super::path_expr::{PathLeg, PathExpression, PATH_EXPR_ASTERISK, PATH_EXPR_ARRAY_INDEX_ASTERISK};

const ESCAPED_UNICODE_BYTES_SIZE: usize = 4;


const CHAR_BACKSPACE: char = '\x08';
const CHAR_HORIZONTAL_TAB: char = '\x09';
const CHAR_LINEFEED: char = '\x0A';
const CHAR_FORMFEED: char = '\x0C';
const CHAR_CARRIAGE_RETURN: char = '\x0D';

impl Json {
    // extract receives several path expressions as arguments, matches them in j, and returns
    // the target JSON matched any path expressions, which may be autowrapped as an array.
    // If there is no any expression matched, it returns None.
    pub fn extract(&self, path_expr_list: &[PathExpression]) -> Option<Json> {
        let mut elem_list = Vec::with_capacity(path_expr_list.len());
        for path_expr in path_expr_list {
            elem_list.append(&mut extract_json(self, &path_expr.legs))
        }
        if elem_list.is_empty() {
            return None;
        }
        if path_expr_list.len() == 1 && elem_list.len() == 1 {
            // If path_expr contains asterisks, elem_list.len() won't be 1
            // even if path_expr_list.len() equals to 1.
            return Some(elem_list.remove(0));
        }
        Some(Json::Array(elem_list))
    }

    pub fn unquote(&self) -> Result<String> {
        match *self {
            Json::String(ref s) => unquote_string(s),
            _ => Ok(self.to_string()),
        }
    }
}

// unquote_string recognizes the escape sequences shown in:
// https://dev.mysql.com/doc/refman/5.7/en/json-modification-functions.html#
// json-unquote-character-escape-sequences
pub fn unquote_string(s: &str) -> Result<String> {
    let mut ret = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let c = match chars.next() {
                Some(c) => c,
                None => return Err(box_err!("Incomplete escaped sequence")),
            };
            match c {
                '"' => ret.push('"'),
                'b' => ret.push(CHAR_BACKSPACE),
                'f' => ret.push(CHAR_FORMFEED),
                'n' => ret.push(CHAR_LINEFEED),
                'r' => ret.push(CHAR_CARRIAGE_RETURN),
                't' => ret.push(CHAR_HORIZONTAL_TAB),
                '\\' => ret.push('\\'),
                'u' => {
                    let b = chars.as_str().as_bytes();
                    if b.len() < ESCAPED_UNICODE_BYTES_SIZE {
                        return Err(box_err!("Invalid unicode, byte len too short: {:?}", b));
                    }
                    let unicode = try!(str::from_utf8(&b[0..ESCAPED_UNICODE_BYTES_SIZE]));
                    if unicode.len() != ESCAPED_UNICODE_BYTES_SIZE {
                        return Err(box_err!("Invalid unicode, char len too short: {}", unicode));
                    }
                    let utf8 = try!(decode_escaped_unicode(unicode));
                    ret.push(utf8);
                    for _ in 0..ESCAPED_UNICODE_BYTES_SIZE {
                        chars.next();
                    }
                }
                _ => {
                    // For all other escape sequences, backslash is ignored.
                    ret.push(c);
                }
            }
        } else {
            ret.push(ch);
        }
    }
    Ok(ret)
}

fn decode_escaped_unicode(s: &str) -> Result<char> {
    let u = box_try!(u32::from_str_radix(s, 16));
    char::from_u32(u).ok_or(box_err!("invalid char from: {}", s))
}

// extract_json is used by JSON::extract().
pub fn extract_json(j: &Json, path_legs: &[PathLeg]) -> Vec<Json> {
    if path_legs.is_empty() {
        return vec![j.clone()];
    }
    let (current_leg, sub_path_legs) = (&path_legs[0], &path_legs[1..]);
    let mut ret = vec![];
    match *current_leg {
        PathLeg::Index(i) => {
            match *j {
                Json::Array(ref array) => {
                    if i == PATH_EXPR_ARRAY_INDEX_ASTERISK {
                        for child in array {
                            ret.append(&mut extract_json(child, sub_path_legs))
                        }
                    } else if (i as usize) < array.len() {
                        ret.append(&mut extract_json(&array[i as usize], sub_path_legs))
                    }
                }
                _ => {
                    if (i == PATH_EXPR_ARRAY_INDEX_ASTERISK) || (i as usize == 0) {
                        ret.append(&mut extract_json(j, sub_path_legs))
                    }
                }
            }
        }
        PathLeg::Key(ref key) => {
            if let Json::Object(ref map) = *j {
                if key == PATH_EXPR_ASTERISK {
                    for key in map.keys() {
                        ret.append(&mut extract_json(&map[key], sub_path_legs))
                    }
                } else if map.contains_key(key) {
                    ret.append(&mut extract_json(&map[key], sub_path_legs))
                }
            }
        }
        PathLeg::DoubleAsterisk => {
            ret.append(&mut extract_json(j, sub_path_legs));
            match *j {
                Json::Array(ref array) => {
                    for child in array {
                        ret.append(&mut extract_json(child, path_legs))
                    }
                }
                Json::Object(ref map) => {
                    for key in map.keys() {
                        ret.append(&mut extract_json(&map[key], path_legs))
                    }
                }
                _ => {}
            }
        }
    }
    ret
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::collections::BTreeMap;
    use super::*;
    use super::super::path_expr::{PathExpressionFlag, PATH_EXPR_ARRAY_INDEX_ASTERISK,
                                  PATH_EXPRESSION_CONTAINS_ASTERISK,
                                  PATH_EXPRESSION_CONTAINS_DOUBLE_ASTERISK};

    #[test]
    fn test_json_extract() {
        let mut test_cases = vec![// no path expression
            ("null", vec![], None),
            // Index
            ("[true, 2017]",
             vec![PathExpression {
                 legs: vec![PathLeg::Index(0)],
                 flags: PathExpressionFlag::default(),
             }],
             Some("true")),
            ("[true, 2017]",
             vec![PathExpression {
                 legs: vec![PathLeg::Index(PATH_EXPR_ARRAY_INDEX_ASTERISK)],
                 flags: PATH_EXPRESSION_CONTAINS_ASTERISK,
             }],
             Some("[true, 2017]")),
            ("[true, 2107]",
             vec![PathExpression {
                 legs: vec![PathLeg::Index(2)],
                 flags: PathExpressionFlag::default(),
             }],
             None),
            ("6.18",
             vec![PathExpression {
                 legs: vec![PathLeg::Index(0)],
                 flags: PathExpressionFlag::default(),
             }],
             Some("6.18")),
            // Key
            (r#"{"a": "a1", "b": 20.08, "c": false}"#,
             vec![PathExpression {
                 legs: vec![PathLeg::Key(String::from("c"))],
                 flags: PathExpressionFlag::default(),
             }],
             Some("false")),
            (r#"{"a": "a1", "b": 20.08, "c": false}"#,
             vec![PathExpression {
                  legs: vec![PathLeg::Key(String::from(PATH_EXPR_ASTERISK))],
                  flags: PATH_EXPRESSION_CONTAINS_ASTERISK,
             }],
             Some(r#"["a1", 20.08, false]"#)),
            (r#"{"a": "a1", "b": 20.08, "c": false}"#,
              vec![PathExpression {
                  legs: vec![PathLeg::Key(String::from("d"))],
                  flags: PathExpressionFlag::default(),
              }],
              None),
             // Double asterisks
             ("21",
              vec![PathExpression {
                  legs: vec![PathLeg::DoubleAsterisk, PathLeg::Key(String::from("c"))],
                  flags: PATH_EXPRESSION_CONTAINS_DOUBLE_ASTERISK,
              }],
              None),
             (r#"{"g": {"a": "a1", "b": 20.08, "c": false}}"#,
              vec![PathExpression {
                  legs: vec![PathLeg::DoubleAsterisk, PathLeg::Key(String::from("c"))],
                  flags: PATH_EXPRESSION_CONTAINS_DOUBLE_ASTERISK,
              }],
              Some("false")),
             (r#"[{"a": "a1", "b": 20.08, "c": false}, true]"#,
              vec![PathExpression {
                  legs: vec![PathLeg::DoubleAsterisk, PathLeg::Key(String::from("c"))],
                  flags: PATH_EXPRESSION_CONTAINS_DOUBLE_ASTERISK,
              }],
              Some("false")),
            ];
        for (i, (js, exprs, expected)) in test_cases.drain(..).enumerate() {
            let j = js.parse();
            assert!(j.is_ok(), "#{} expect parse ok but got {:?}", i, j);
            let j: Json = j.unwrap();
            let expected = match expected {
                Some(es) => {
                    let e = Json::from_str(es);
                    assert!(e.is_ok(), "#{} expect parse json ok but got {:?}", i, e);
                    Some(e.unwrap())
                }
                None => None,
            };
            let got = j.extract(&exprs[..]);
            assert_eq!(got,
                       expected,
                       "#{} expect {:?}, but got {:?}",
                       i,
                       expected,
                       got);
        }
    }

    #[test]
    fn test_decode_escaped_unicode() {
        let mut test_cases = vec![
                ("5e8a", '床'),
                ("524d", '前'),
                ("660e", '明'),
                ("6708", '月'),
                ("5149", '光'),
            ];
        for (i, (escaped, expected)) in test_cases.drain(..).enumerate() {
            let d = decode_escaped_unicode(escaped);
            assert!(d.is_ok(), "#{} expect ok but got err {:?}", i, d);
            let got = d.unwrap();
            assert_eq!(got,
                       expected,
                       "#{} expect {:?} but got {:?}",
                       i,
                       expected,
                       got);
        }
    }

    #[test]
    fn test_json_unquote() {
        // test unquote json string
        let mut test_cases = vec![("\\b", true, Some("\x08")),
                                  ("\\f", true, Some("\x0C")),
                                  ("\\n", true, Some("\x0A")),
                                  ("\\r", true, Some("\x0D")),
                                  ("\\t", true, Some("\x09")),
                                  ("\\\\", true, Some("\x5c")),
                                  ("\\u597d", true, Some("好")),
                                  ("0\\u597d0", true, Some("0好0")),
                                  ("\\a", true, Some("a")),
                                  ("[", true, Some("[")),
                                  // invalid input
                                  ("\\", false, None),
                                  ("\\u59", false, None)];
        for (i, (input, no_error, expected)) in test_cases.drain(..).enumerate() {
            let j = Json::String(String::from(input));
            let r = j.unquote();
            if no_error {
                assert!(r.is_ok(), "#{} expect unquote ok but got err {:?}", i, r);
                let got = r.unwrap();
                let expected = String::from(expected.unwrap());
                assert_eq!(got,
                           expected,
                           "#{} expect {:?} but got {:?}",
                           i,
                           expected,
                           got);
            } else {
                assert!(r.is_err(), "#{} expected error but got {:?}", i, r);
            }
        }

        // test unquote other json types
        let mut test_cases = vec![Json::Object(BTreeMap::new()),
                                  Json::Array(vec![]),
                                  Json::I64(2017),
                                  Json::Double(19.28),
                                  Json::Boolean(true),
                                  Json::None];
        for (i, j) in test_cases.drain(..).enumerate() {
            let expected = j.to_string();
            let r = j.unquote();
            assert!(r.is_ok(), "#{} expect unquote ok but got err {:?}", i, r);
            let got = r.unwrap();
            assert_eq!(got,
                       expected,
                       "#{} expect {:?} but got {:?}",
                       i,
                       expected,
                       got);
        }
    }
}
