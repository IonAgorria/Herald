use serde::{ser, Serialize};

use crate::error::{Error, Result};

pub struct Serializer {
    key_mode: bool,
    output: String,
}

// By convention, the public API of a Serde serializer is one or more `to_abc`
// functions such as `to_string`, `to_bytes`, or `to_writer` depending on what
// Rust types the serializer is able to produce as output.
//
// This basic serializer supports only `to_string`.
pub fn to_string<T>(value: &T) -> Result<String>
    where
        T: Serialize,
{
    let mut serializer = Serializer {
        key_mode: false,
        output: String::new(),
    };
    value.serialize(&mut serializer)?;
    //Apparently root XPrm objects don't have brackets, therefore remove them
    Ok(serializer.output)
}

//XPrm structure related helpers
impl Serializer {
    fn open_bracket(&mut self) {
        self.output += "{";
    }
    fn close_bracket(&mut self) {
        self.output += "}";
    }
    fn open_structure(&mut self) {
        self.open_bracket();
    }
    fn close_structure(&mut self) {
        self.close_bracket();
    }
}

impl<'a> ser::Serializer for &'a mut Serializer {
    // The output type produced by this `Serializer` during successful
    // serialization. Most serializers that produce text or binary output should
    // set `Ok = ()` and serialize into an `io::Write` or buffer contained
    // within the `Serializer` instance, as happens here. Serializers that build
    // in-memory data structures may be simplified by using `Ok` to propagate
    // the data structure around.
    type Ok = ();

    // The error type when some error occurs during serialization.
    type Error = Error;

    // Associated types for keeping track of additional state while serializing
    // compound data structures like sequences and maps. In this case no
    // additional state is required beyond what is already stored in the
    // Serializer struct.
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;
    
    //Actual serialization funcs

    fn serialize_bool(self, v: bool) -> Result<()> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        self.output += if v { "true" } else { "false" };
        Ok(())
    }

    fn serialize_i8(self, v: i8) -> Result<()> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_i16(self, v: i16) -> Result<()> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_i32(self, v: i32) -> Result<()> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_i64(self, v: i64) -> Result<()> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        self.output += &v.to_string();
        Ok(())
    }

    fn serialize_u8(self, v: u8) -> Result<()> {
        self.serialize_u64(u64::from(v))
    }

    fn serialize_u16(self, v: u16) -> Result<()> {
        self.serialize_u64(u64::from(v))
    }

    fn serialize_u32(self, v: u32) -> Result<()> {
        self.serialize_u64(u64::from(v))
    }

    fn serialize_u64(self, v: u64) -> Result<()> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        self.output += &v.to_string();
        Ok(())
    }

    fn serialize_f32(self, v: f32) -> Result<()> {
        self.serialize_f64(f64::from(v))
    }

    fn serialize_f64(self, v: f64) -> Result<()> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        self.output += &v.to_string();
        Ok(())
    }

    // Serialize a char as a single-character string. Other formats may
    // represent this differently.
    fn serialize_char(self, v: char) -> Result<()> {
        self.serialize_str(&v.to_string())
    }

    fn serialize_str(self, v: &str) -> Result<()> {
        if self.key_mode {
            for char in v.chars() {
                if !char.is_ascii_alphanumeric() && char != '_' {
                    let mut err = String::from("Key has invalid char: '");
                    err.push(char);
                    err.push_str("'");
                    return Err(Error::Message(err));
                }
            }
            self.output += v;
        } else {
            self.output += "\"";
            for c in v.chars() {
                if c == '\\' {
                    self.output += "\\\\";
                } else if c == '\n' {
                    self.output += "\\n";
                } else if c == '\r' {
                    self.output += "\\r";
                } else if c == '"' {
                    self.output += "\\\"";
                } else {
                    self.output += c.to_string().as_str();
                }
            }
            self.output += "\"";
        }
        Ok(())
    }

    // Serialize a byte array as an array of bytes.
    fn serialize_bytes(self, v: &[u8]) -> Result<()> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        use serde::ser::SerializeSeq;
        let mut seq = self.serialize_seq(Some(v.len()))?;
        for byte in v {
            seq.serialize_element(byte)?;
        }
        seq.end()
    }

    fn serialize_none(self) -> Result<()> {
        self.serialize_unit()
    }

    fn serialize_some<T>(self, value: &T) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<()> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        self.output += "0";
        Ok(())
    }

    // Unit struct means a named value containing no data.
    fn serialize_unit_struct(self, name: &'static str) -> Result<()> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        self.output += name;
        self.output += "=0";
        Ok(())
    }

    // When serializing a unit variant (or any other kind of variant), formats
    // can choose whether to keep track of it by index or by name. Binary
    // formats typically use the index of the variant and human-readable formats
    // typically use the name.
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<()> {
        self.output += variant;
        Ok(())
    }

    // As is done here, serializers are encouraged to treat newtype structs as
    // insignificant wrappers around the data they contain.
    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        Err(Error::Message(format!("NewtypeVariant for {:?} not supported", name)))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        self.open_bracket();
        if let Some(l) = len {
            self.output += &l.to_string();
            self.output += ";";
            Ok(self)
        } else { 
            Err(Error::Message("Unknown seq size".to_string()))
        }
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        Err(Error::Message(format!("TupleVariant for {:?} not supported", name)))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        self.open_structure();
        Ok(self)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct> {
        self.serialize_map(Some(len))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        if self.key_mode {
            return Err(Error::Message("Key must be string".into()));
        }
        self.open_structure();
        self.output += variant;
        self.output += "=";
        self.open_structure();
        Ok(self)
    }
}

// The following 7 impls deal with the serialization of compound types like
// sequences and maps. Serialization of such types is begun by a Serializer
// method and followed by zero or more calls to serialize individual elements of
// the compound type and one call to end the compound type.
//
// This impl is SerializeSeq so these methods are called after `serialize_seq`
// is called on the Serializer.
impl<'a> ser::SerializeSeq for &'a mut Serializer {
    // Must match the `Ok` type of the serializer.
    type Ok = ();
    // Must match the `Error` type of the serializer.
    type Error = Error;

    // Serialize a single element of the sequence.
    fn serialize_element<T>(&mut self, value: &T) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        if !self.output.ends_with(';') {
            self.output += ",";
        }
        value.serialize(&mut **self)
    }

    // Close the sequence.
    fn end(self) -> Result<()> {
        self.close_bracket();
        Ok(())
    }
}

// Same thing but for tuples.
impl<'a> ser::SerializeTuple for &'a mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        value.serialize(&mut **self)?;
        Ok(())
    }

    fn end(self) -> Result<()> {
        self.close_bracket();
        Ok(())
    }
}

// Same thing but for tuple structs.
impl<'a> ser::SerializeTupleStruct for &'a mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        if !self.output.ends_with(';') {
            self.output += ",";
        }
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        self.close_bracket();
        Ok(())
    }
}

impl<'a> ser::SerializeTupleVariant for &'a mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, _value: &T) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        Err(Error::Message("TupleVariant not supported".to_string()))
    }

    fn end(self) -> Result<()> {
        Err(Error::Message("TupleVariant not supported".to_string()))
    }
}

impl<'a> ser::SerializeMap for &'a mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T>(&mut self, key: &T) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        self.key_mode = true;
        key.serialize(&mut **self)?;
        self.key_mode = false;
        Ok(())
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        self.output += "=";
        value.serialize(&mut **self)?;
        self.output += ";";
        Ok(())
    }

    fn end(self) -> Result<()> {
        self.close_structure();
        Ok(())
    }
}

// Structs are like maps in which the keys are constrained to be compile-time
// constant strings.
impl<'a> ser::SerializeStruct for &'a mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        self.key_mode = true;
        key.serialize(&mut **self)?;
        self.key_mode = false;
        self.output += "=";
        value.serialize(&mut **self)?;
        self.output += ";";
        Ok(())
    }

    fn end(self) -> Result<()> {
        self.close_structure();
        Ok(())
    }
}

// Similar to `SerializeTupleVariant`, here the `end` method is responsible for
// closing both of the curly braces opened by `serialize_struct_variant`.
impl<'a> ser::SerializeStructVariant for &'a mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<()>
        where
            T: ?Sized + Serialize,
    {
        self.key_mode = true;
        key.serialize(&mut **self)?;
        self.key_mode = false;
        self.output += "=";
        value.serialize(&mut **self)?;
        self.output += ";";
        Ok(())
    }

    fn end(self) -> Result<()> {
        self.close_structure();
        self.output += ";";
        self.close_structure();
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////

#[test]
fn test_struct() {
    //Tuple struct
    #[derive(Serialize)]
    struct Test3(u8,u32);
    //Newtype struct
    #[derive(Serialize)]
    struct Test4(u8);
    //Struct
    #[derive(Serialize)]
    struct Test2 {
        int: u32,
        flt: f32,
        opt: Option<f32>,
    }
    //Struct
    #[derive(Serialize)]
    struct Test {
        //Substructs
        ts2: Test2,
        ts3: Test3,
        //Optional with value
        ts4: Option<Test4>,
        //None/null
        nll: Option<Test2>,
        //Seq
        seq: Vec<&'static str>,
        //Empty seq
        sem: Vec<&'static str>,
    }

    let test = Test {
        ts2: Test2 {
            int: 1,
            flt: 1.5f32,
            opt: None,
        },
        ts3: Test3(1, 2),
        ts4: Some(Test4(4)),
        nll: None,
        seq: vec!["a", "b"],
        sem: vec![],
    };
    let expected = r#"{ts2={int=1;flt=1.5;opt=0;};ts3={2;1,2};ts4=4;nll=0;seq={2;"a","b"};sem={0;};}"#;
    assert_eq!(to_string(&test).unwrap(), expected);
}

#[test]
fn test_enum() {
    #[derive(Serialize)]
    enum E {
        Unit,
        Newtype(u32),
        Tuple(u32, u32),
        Struct { a: u32 },
    }

    let u = E::Unit;
    let expected = r#"Unit"#;
    assert_eq!(to_string(&u).unwrap(), expected);

    let n = E::Newtype(1);
    assert!(to_string(&n).is_err());

    let t = E::Tuple(1, 2);
    assert!(to_string(&t).is_err());

    let s = E::Struct { a: 1 };
    let expected = r#"{Struct={a=1;};}"#;
    assert_eq!(to_string(&s).unwrap(), expected);
}

#[test]
fn test_escape() {
    #[derive(Serialize)]
    struct Test(String);

    let s = Test("Data \" \\ \r\n".to_string());

    let expected = r#""Data \" \\ \r\n""#;
    let result = to_string(&s).unwrap();
    assert_eq!(result, expected);
}

#[test]
fn test_map() {
    let map = std::collections::BTreeMap::from([
        ("aaa", "value"),
        ("other", "content"),
    ]);

    let expected = r#"{aaa="value";other="content";}"#;
    let result = to_string(&map).unwrap();
    assert_eq!(result, expected);
}
