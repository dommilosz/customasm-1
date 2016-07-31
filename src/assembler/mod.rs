mod labels;


use util::parser::{Parser, ParserError};
use util::tokenizer;
use util::tokenizer::Span;
use util::bitvec::BitVec;
use definition::Definition;
use rule::{PatternSegment, ProductionSegment};


struct Assembler<'def>
{
	def: &'def Definition,
	cur_address: usize,
	cur_output: usize,
	labels: labels::Manager,
	unresolved_instructions: Vec<Instruction>,
	unresolved_expressions: Vec<UnresolvedExpression>,
	
	output_bits: BitVec
}


struct Instruction
{
	span: Span,
	rule_index: usize,
	address: usize,
	output: usize,
	arguments: Vec<Expression>
}


struct UnresolvedExpression
{
	span: Span,
	expr: Expression,
	address: usize,
	output: usize,
	bit_num: usize
}


enum Expression
{
	LiteralUInt(BitVec),
	GlobalLabel(String),
	LocalLabel(labels::Context, String)
}


pub fn assemble(def: &Definition, src: &[char]) -> Result<BitVec, ParserError>
{
	let mut assembler = Assembler
	{
		def: def,
		cur_address: 0,
		cur_output: 0,
		labels: labels::Manager::new(),
		unresolved_instructions: Vec::new(),
		unresolved_expressions: Vec::new(),
		output_bits: BitVec::new()
	};
	
	let tokens = tokenizer::tokenize(src);
	let mut parser = Parser::new(&tokens);
	
	while !parser.is_over()
	{
		if parser.current().is_operator(".")
			{ try!(translate_directive(&mut assembler, &mut parser)); }
		else if parser.current().is_identifier() && parser.next(1).is_operator(":")
			{ try!(translate_global_label(&mut assembler, &mut parser)); }
		else if parser.current().is_operator("'") && parser.next(1).is_identifier() && parser.next(2).is_operator(":")
			{ try!(translate_local_label(&mut assembler, &mut parser)); }
		else
			{ try!(translate_instruction(&mut assembler, &mut parser)); }
	}
	
	let mut unresolved_insts = Vec::new();
	unresolved_insts.append(&mut assembler.unresolved_instructions);
	
	for inst in unresolved_insts.iter()
	{
		match resolve_instruction(&assembler, &inst)
		{
			Ok(bits) => assembler.output_aligned(inst.output, &bits),			
			Err(msg) => return Err(ParserError::new(msg, inst.span))
		}
	}
	
	let mut unresolved_exprs = Vec::new();
	unresolved_exprs.append(&mut assembler.unresolved_expressions);
	
	for unres in unresolved_exprs.iter()
	{
		match resolve_expression(&assembler, &unres.expr)
		{
			Ok(bits) => assembler.output_aligned(unres.output, &bits.slice(unres.bit_num - 1, 0)),
			Err(msg) => return Err(ParserError::new(msg, unres.span))
		}
	}
	
	Ok(assembler.output_bits)
}


impl<'def> Assembler<'def>
{
	pub fn output_aligned(&mut self, index: usize, bitvec: &BitVec)
	{
		let aligned_index = index * self.def.align_bits;
		self.output_bits.set(aligned_index, bitvec);
	}
}


fn translate_directive(assembler: &mut Assembler, parser: &mut Parser) -> Result<(), ParserError>
{
	try!(parser.expect_operator("."));
	let directive_token = try!(parser.expect_identifier()).clone();
	let directive = directive_token.identifier();
	
	if directive.chars().next() == Some('d')
	{
		let mut bit_num_str = directive.clone();
		bit_num_str.remove(0);
		
		match usize::from_str_radix(&bit_num_str, 10)
		{
			Ok(bit_num) =>
			{
				if bit_num % assembler.def.align_bits != 0
					{ return Err(ParserError::new(format!("literal is not aligned to `{}` bits", assembler.def.align_bits), directive_token.span)); }
			
				return translate_literal(assembler, parser, bit_num);
			}
			Err(..) => { }
		}
	}
	
	match directive.as_ref()
	{
		"address" => assembler.cur_address = try!(parser.expect_number()).number_usize(),
		"output" => assembler.cur_output = try!(parser.expect_number()).number_usize(),
		_ => return Err(ParserError::new(format!("unknown directive `{}`", directive), directive_token.span))
	}
	
	try!(parser.expect_separator_linebreak());
	Ok(())
}


fn translate_literal(assembler: &mut Assembler, parser: &mut Parser, bit_num: usize) -> Result<(), ParserError>
{
	loop
	{
		let span = parser.current().span;
		let expr = try!(parse_expression(assembler, parser));
		
		if can_resolve_expression(assembler, &expr)
		{
			let bits = match resolve_expression(assembler, &expr)
			{
				Ok(bits) => bits,
				Err(msg) => return Err(ParserError::new(msg, span))
			};
			
			let cur_output = assembler.cur_output;
			assembler.output_aligned(cur_output, &bits.slice(bit_num - 1, 0));
			
		}
		else
		{
			assembler.unresolved_expressions.push(UnresolvedExpression
			{
				span: span,
				expr: expr,
				address: assembler.cur_address,
				output: assembler.cur_output,
				bit_num: bit_num
			});
		}
		
		assembler.cur_address += bit_num / assembler.def.align_bits;
		assembler.cur_output += bit_num / assembler.def.align_bits;
		
		if !parser.match_operator(",")
			{ break; }
	}
	
	try!(parser.expect_separator_linebreak());
	Ok(())
}


fn translate_global_label(assembler: &mut Assembler, parser: &mut Parser) -> Result<(), ParserError>
{
	let label_token = try!(parser.expect_identifier()).clone();
	let label = label_token.identifier();
	try!(parser.expect_operator(":"));
	
	if assembler.labels.does_global_exist(label)
		{ return Err(ParserError::new(format!("duplicate global label `{}`", label), label_token.span)); }
	
	assembler.labels.add_global(
		label.clone(),
		BitVec::new_from_usize(assembler.cur_address));
	
	try!(parser.expect_separator_linebreak());
	Ok(())
}


fn translate_local_label(assembler: &mut Assembler, parser: &mut Parser) -> Result<(), ParserError>
{
	try!(parser.expect_operator("'"));
	let label_token = try!(parser.expect_identifier()).clone();
	let label = label_token.identifier();
	try!(parser.expect_operator(":"));
	
	if assembler.labels.does_local_exist(assembler.labels.get_cur_context(), label)
		{ return Err(ParserError::new(format!("duplicate local label `{}`", label), label_token.span)); }
	
	let local_ctx = assembler.labels.get_cur_context();
	assembler.labels.add_local(
		local_ctx,
		label.clone(),
		BitVec::new_from_usize(assembler.cur_address));
	
	try!(parser.expect_separator_linebreak());
	Ok(())
}


fn translate_instruction<'p, 'tok>(assembler: &mut Assembler, parser: &'p mut Parser<'tok>) -> Result<(), ParserError>
{
	let mut maybe_inst = None;
	let inst_span = parser.current().span;

	for rule_index in 0..assembler.def.rules.len()
	{
		let mut rule_parser = parser.clone_from_current();
		
		match try_match_rule(assembler, &mut rule_parser, rule_index)
		{
			Some(inst) =>
			{
				if can_resolve_instruction(assembler, &inst)
				{
					maybe_inst = Some(inst);
					*parser = rule_parser;
					break;
				}
				else
				{
					maybe_inst = Some(inst);
					*parser = rule_parser;
				}
			}
			None => { }
		}
	}
	
	match maybe_inst
	{
		Some(inst) =>
		{
			let rule = &assembler.def.rules[inst.rule_index];
			assembler.cur_address += rule.production_bit_num / assembler.def.align_bits;
			assembler.cur_output += rule.production_bit_num / assembler.def.align_bits;
			
			if can_resolve_instruction(assembler, &inst)
			{
				match resolve_instruction(assembler, &inst)
				{
					Ok(bits) => assembler.output_aligned(inst.output, &bits),					
					Err(msg) => return Err(ParserError::new(msg, inst.span))
				}
			}
			else
			{
				assembler.unresolved_instructions.push(inst);
			}
		}
	
		None => return Err(ParserError::new("no match found for instruction".to_string(), inst_span))
	}
	
	try!(parser.expect_separator_linebreak());
	Ok(())
}


fn try_match_rule(assembler: &mut Assembler, parser: &mut Parser, rule_index: usize) -> Option<Instruction>
{
	let rule = &assembler.def.rules[rule_index];
	
	let mut inst = Instruction
	{
		span: parser.current().span,
		rule_index: rule_index,
		address: assembler.cur_address,
		output: assembler.cur_output,
		arguments: Vec::new()
	};
	
	for segment in rule.pattern_segments.iter()
	{
		match segment
		{
			&PatternSegment::Exact(ref chars) =>
			{
				if parser.current().is_identifier() && parser.current().identifier() == chars
					{ parser.advance(); }
				else if parser.current().is_operator(&chars)
					{ parser.advance(); }
				else
					{ return None; }
			}
			
			&PatternSegment::Argument(arg_index) =>
			{
				let typ = rule.get_argument_type(arg_index);
				
				match parse_expression(assembler, parser)
				{
					Ok(expr) =>
					{
						let expr_len = get_expression_min_bit_num(assembler, &expr);
						if expr_len <= typ.bit_num
							{ inst.arguments.push(expr); }
						else
							{ return None; }
					}
					Err(..) => return None
				};
			}
		}
	}
	
	Some(inst)
}


fn parse_expression(assembler: &Assembler, parser: &mut Parser) -> Result<Expression, ParserError>
{
	if parser.current().is_identifier()
		{ Ok(Expression::GlobalLabel(try!(parser.expect_identifier()).identifier().clone())) }
	
	else if parser.current().is_operator("'") && parser.next(1).is_identifier()
	{
		parser.advance();
		Ok(Expression::LocalLabel(
			assembler.labels.get_cur_context(),
			try!(parser.expect_identifier()).identifier().clone()))
	}
	
	else if parser.current().is_number()
	{
		let token = try!(parser.expect_number());
		let (radix, value_str) = token.number();
		
		match BitVec::new_from_str_min(radix, value_str)
		{
			Err(msg) => Err(ParserError::new(msg, token.span)),
			Ok(bitvec) => Ok(Expression::LiteralUInt(bitvec))
		}
	}
	
	else
		{ Err(ParserError::new("expected expression".to_string(), parser.current().span)) }
}


fn can_resolve_instruction(assembler: &Assembler, inst: &Instruction) -> bool
{
	for segment in assembler.def.rules[inst.rule_index].production_segments.iter()
	{
		match segment
		{
			&ProductionSegment::Literal(..) => { }
			
			&ProductionSegment::Argument { index, leftmost_bit: _, rightmost_bit: _ } =>
			{
				if !can_resolve_expression(assembler, &inst.arguments[index])
					{ return false; }
			}
		}
	}
	
	true
}


fn resolve_instruction(assembler: &Assembler, inst: &Instruction) -> Result<BitVec, String>
{
	let mut bitvec = BitVec::new();
	
	for segment in assembler.def.rules[inst.rule_index].production_segments.iter()
	{
		match segment
		{
			&ProductionSegment::Literal(ref literal_bitvec) =>
			{
				bitvec.push(&literal_bitvec);
			}
			
			&ProductionSegment::Argument { index, leftmost_bit, rightmost_bit } =>
			{
				let expr_bitvec = try!(resolve_expression(assembler, &inst.arguments[index]));
				bitvec.push(&expr_bitvec.slice(leftmost_bit, rightmost_bit));
			}
		}
	}
	
	Ok(bitvec)
}


fn can_resolve_expression(assembler: &Assembler, expr: &Expression) -> bool
{
	match expr
	{
		&Expression::LiteralUInt(..) =>
			{ return true; }
		
		&Expression::GlobalLabel(ref name) =>
			{ return assembler.labels.does_global_exist(name); }
			
		&Expression::LocalLabel(ctx, ref name) =>
			{ return assembler.labels.does_local_exist(ctx, name); }
	}
}


fn get_expression_min_bit_num(assembler: &Assembler, expr: &Expression) -> usize
{
	match expr
	{
		&Expression::LiteralUInt(ref literal_bitvec) => return literal_bitvec.len(),
		&Expression::GlobalLabel(ref name) =>
		{
			match assembler.labels.get_global_value(name)
			{
				Some(bitvec) => bitvec.len(),
				None => assembler.def.address_bits
			}
		}
		&Expression::LocalLabel(ctx, ref name) =>
		{
			match assembler.labels.get_local_value(ctx, name)
			{
				Some(bitvec) => bitvec.len(),
				None => assembler.def.address_bits
			}
		}
	}
}


fn resolve_expression(assembler: &Assembler, expr: &Expression) -> Result<BitVec, String>
{
	match expr
	{
		&Expression::LiteralUInt(ref literal_bitvec) =>
		{
			return Ok(literal_bitvec.clone());
		}
		
		&Expression::GlobalLabel(ref name) =>
		{
			match assembler.labels.get_global_value(name)
			{
				Some(value) => return Ok(value.clone()),
				None => return Err(format!("unknown global label `{}`", name))
			}
		}
		
		&Expression::LocalLabel(ctx, ref name) =>
		{
			match assembler.labels.get_local_value(ctx, name)
			{
				Some(value) => return Ok(value.clone()),
				None => return Err(format!("unknown local label `{}`", name))
			}
		}
	}
}